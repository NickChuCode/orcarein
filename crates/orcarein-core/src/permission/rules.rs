//! allow/ask/deny permission rule engine.
//!
//! A [`Ruleset`] maps a `(tool, [bash command], [file paths])` request to an
//! [`Action`] (Allow / Ask / Deny). Rules come from the user's `config.toml`; a
//! built-in set of sensitive-path defaults (escalating `.env`, SSH keys, etc.
//! to `Ask`) is always present. Deny wins over everything; an explicit user
//! `allow` wins over the sensitive defaults — hence the two-tier split.
//!
//! Pure and silent (no tracing) — the interactive prompt and config
//! persistence live in the binary.

use serde::{Deserialize, Serialize};

use crate::tool::RiskLevel;

/// The gate outcome for one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    Ask,
    Deny,
}

/// A rule's action as written in `config.toml` (`allow` / `ask` / `deny`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    Allow,
    Ask,
    Deny,
}

/// One permission rule. `tool` is a name or `"*"`; at most one of `command`
/// (bash) / `path` (file tools) narrows the match; both absent = "any use of
/// this tool".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub action: RuleAction,
}

/// A permission request distilled from a tool call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionRequest {
    pub tool: String,
    pub bash_command: Option<String>,
    pub paths: Vec<String>,
}

/// A two-tier ruleset: user rules (config) are evaluated before the built-in
/// sensitive-path defaults, so an explicit user `allow` beats a default `ask`.
pub struct Ruleset {
    user_rules: Vec<PermissionRule>,
    default_rules: Vec<PermissionRule>,
}

impl Ruleset {
    /// Empty user rules + built-in sensitive-path defaults.
    pub fn with_defaults() -> Self {
        Ruleset {
            user_rules: Vec::new(),
            default_rules: sensitive_defaults(),
        }
    }

    /// User rules from config + built-in sensitive-path defaults.
    pub fn from_config(user_rules: Vec<PermissionRule>) -> Self {
        Ruleset {
            user_rules,
            default_rules: sensitive_defaults(),
        }
    }

    /// Resolve the action for `req`. Pure. Four steps (spec §3.2):
    /// 1. deny across both tiers → Deny.
    /// 2. user rules: ask then allow → first match.
    /// 3. default rules: ask → Ask.
    /// 4. base_risk fallback (Safe→Allow, Risky→Ask).
    pub fn evaluate(&self, req: &PermissionRequest, base_risk: RiskLevel) -> Action {
        // 1. deny wins, from any tier.
        for r in self.user_rules.iter().chain(self.default_rules.iter()) {
            if r.action == RuleAction::Deny && rule_matches(r, req) {
                return Action::Deny;
            }
        }
        // 2. user rules: ask before allow (explicit user allow wins here).
        for want in [RuleAction::Ask, RuleAction::Allow] {
            for r in &self.user_rules {
                if r.action == want && rule_matches(r, req) {
                    return to_action(want);
                }
            }
        }
        // 3. sensitive defaults (ask-only).
        for r in &self.default_rules {
            if r.action == RuleAction::Ask && rule_matches(r, req) {
                return Action::Ask;
            }
        }
        // 4. base risk.
        match base_risk {
            RiskLevel::Risky => Action::Ask,
            RiskLevel::Safe => Action::Allow,
        }
    }
}

fn to_action(a: RuleAction) -> Action {
    match a {
        RuleAction::Allow => Action::Allow,
        RuleAction::Ask => Action::Ask,
        RuleAction::Deny => Action::Deny,
    }
}

/// Does `rule` match `req`? Tool name must match (`*` = any). Then a `command`
/// rule requires the request's bash command to glob-match; a `path` rule
/// requires at least one request path to glob-match. Neither → any use.
fn rule_matches(rule: &PermissionRule, req: &PermissionRequest) -> bool {
    if rule.tool != "*" && rule.tool != req.tool {
        return false;
    }
    if let Some(cmd_pat) = &rule.command {
        return match &req.bash_command {
            Some(cmd) => glob_match(cmd_pat, cmd),
            None => false,
        };
    }
    if let Some(path_pat) = &rule.path {
        return req.paths.iter().any(|p| glob_match(path_pat, p));
    }
    true
}

/// Built-in sensitive-path defaults: escalate reads/edits of secrets to `Ask`
/// (tool = `*`, action = ask). Hard deny is left to explicit user rules.
fn sensitive_defaults() -> Vec<PermissionRule> {
    [
        ".env",
        ".env.*",
        "*.pem",
        "*.key",
        "id_rsa",
        "id_ed25519",
        "~/.ssh/**",
        "~/.aws/credentials",
    ]
    .iter()
    .map(|p| PermissionRule {
        tool: "*".to_string(),
        command: None,
        path: Some((*p).to_string()),
        action: RuleAction::Ask,
    })
    .collect()
}

/// Distill a permission request from a tool name + parsed args. Knows the
/// built-in tools' arg shapes; unknown / MCP / `task` tools yield tool-name
/// only (never a fabricated path — protects the subagent's Safe posture).
pub fn extract(tool: &str, args: &serde_json::Value) -> PermissionRequest {
    let mut req = PermissionRequest {
        tool: tool.to_string(),
        ..Default::default()
    };
    match tool {
        "bash" => {
            if let Some(c) = args.get("command").and_then(|v| v.as_str()) {
                req.bash_command = Some(c.to_string());
            }
        }
        "read_file" | "write_file" | "edit" | "list_dir" | "search" => {
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                req.paths.push(p.to_string());
            }
        }
        _ => {} // task / skill / MCP / unknown → tool name only
    }
    req
}

/// The user's home directory as a string (empty if it can't be resolved).
fn home_dir() -> String {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `*`-glob (zero-dep). `*` matches any run of chars incl. `/` and empty;
/// everything else is literal. A bare-name pattern (no `/`) also matches the
/// basename of `text` (so `.env` matches `foo/.env`). `~/` is expanded to
/// `home` first. Patterns are ASCII; byte matching is UTF-8-safe because ASCII
/// bytes never occur inside a multibyte sequence.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_with_home(pattern, text, &home_dir())
}

/// `glob_match` with an injected home dir (so tests never touch the real HOME).
///
/// Both the (home-expanded) pattern and the text are normalized to `/`
/// separators before matching, since `directories::BaseDirs` returns
/// backslash-separated paths on Windows (e.g. `C:\Users\name`) while our
/// sensitive-path defaults are written with `/` (e.g. `~/.ssh/**`). Without
/// normalization a real Windows path like `C:\Users\name\.ssh\id_rsa` would
/// never match `~/.ssh/**`, silently disabling those defaults. Over-matching
/// is safe here — a false match only escalates to an `Ask` prompt, never an
/// auto-allow.
pub(crate) fn glob_match_with_home(pattern: &str, text: &str, home: &str) -> bool {
    let expanded;
    let pat = if let Some(rest) = pattern.strip_prefix("~/") {
        expanded = format!("{home}/{rest}");
        expanded.as_str()
    } else {
        pattern
    };
    let pat_norm = pat.replace('\\', "/");
    let text_norm = text.replace('\\', "/");
    if star_match(&pat_norm, &text_norm) {
        return true;
    }
    if !pat_norm.contains('/') {
        let base = text_norm.rsplit('/').next().unwrap_or(&text_norm);
        return star_match(&pat_norm, base);
    }
    false
}

/// Classic greedy `*`-glob matcher with backtracking. No `?`, no char classes
/// (a deliberate subset — spec §7).
fn star_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(tool: &str, cmd: Option<&str>, paths: &[&str]) -> PermissionRequest {
        PermissionRequest {
            tool: tool.to_string(),
            bash_command: cmd.map(String::from),
            paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn rule(
        tool: &str,
        cmd: Option<&str>,
        path: Option<&str>,
        action: RuleAction,
    ) -> PermissionRule {
        PermissionRule {
            tool: tool.to_string(),
            command: cmd.map(String::from),
            path: path.map(String::from),
            action,
        }
    }

    // --- RuleAction serde ---
    #[test]
    fn rule_action_serde_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&RuleAction::Allow).unwrap(),
            "\"allow\""
        );
        assert_eq!(serde_json::to_string(&RuleAction::Ask).unwrap(), "\"ask\"");
        assert_eq!(
            serde_json::to_string(&RuleAction::Deny).unwrap(),
            "\"deny\""
        );

        assert_eq!(
            serde_json::from_str::<RuleAction>("\"allow\"").unwrap(),
            RuleAction::Allow
        );
        assert_eq!(
            serde_json::from_str::<RuleAction>("\"ask\"").unwrap(),
            RuleAction::Ask
        );
        assert_eq!(
            serde_json::from_str::<RuleAction>("\"deny\"").unwrap(),
            RuleAction::Deny
        );
    }

    // --- glob ---
    #[test]
    fn star_glob_basics() {
        assert!(glob_match_with_home("git *", "git status", ""));
        assert!(glob_match_with_home("git *", "git push origin main", ""));
        assert!(!glob_match_with_home("git *", "github-cli", ""));
        assert!(glob_match_with_home("rm *", "rm -rf /", ""));
    }

    #[test]
    fn bare_name_matches_basename() {
        assert!(glob_match_with_home(".env", ".env", ""));
        assert!(glob_match_with_home(".env", "sub/dir/.env", ""));
        assert!(glob_match_with_home("*.pem", "certs/server.pem", ""));
        assert!(!glob_match_with_home(".env", "environment", ""));
    }

    #[test]
    fn tilde_expands_with_injected_home() {
        assert!(glob_match_with_home(
            "~/.ssh/**",
            "/home/u/.ssh/id_rsa",
            "/home/u"
        ));
        assert!(!glob_match_with_home(
            "~/.ssh/**",
            "/home/u/notes.txt",
            "/home/u"
        ));
    }

    #[test]
    fn windows_backslash_paths_match_tilde_defaults() {
        // Regression: directories::BaseDirs returns backslash-separated
        // paths on Windows (e.g. C:\Users\u), so ~/.ssh/** must still match
        // a real Windows path with backslashes throughout.
        assert!(glob_match_with_home(
            "~/.ssh/**",
            "C:\\Users\\u\\.ssh\\id_rsa",
            "C:\\Users\\u"
        ));
        assert!(glob_match_with_home(
            "~/.aws/credentials",
            "C:\\Users\\u\\.aws\\credentials",
            "C:\\Users\\u"
        ));
    }

    // --- evaluate ---
    #[test]
    fn deny_wins_over_allow() {
        let rs = Ruleset::from_config(vec![
            rule("bash", Some("git *"), None, RuleAction::Allow),
            rule("bash", Some("git push*"), None, RuleAction::Deny),
        ]);
        assert_eq!(
            rs.evaluate(&req("bash", Some("git push origin"), &[]), RiskLevel::Risky),
            Action::Deny
        );
        assert_eq!(
            rs.evaluate(&req("bash", Some("git status"), &[]), RiskLevel::Risky),
            Action::Allow
        );
    }

    #[test]
    fn ask_wins_over_allow() {
        let rs = Ruleset::from_config(vec![
            rule("bash", Some("git *"), None, RuleAction::Allow),
            rule("bash", Some("git push*"), None, RuleAction::Ask),
        ]);
        assert_eq!(
            rs.evaluate(&req("bash", Some("git push"), &[]), RiskLevel::Risky),
            Action::Ask
        );
    }

    #[test]
    fn base_risk_fallback_is_backward_compatible() {
        let rs = Ruleset::with_defaults();
        // Safe non-sensitive → Allow (no prompt); Risky no-rule → Ask.
        assert_eq!(
            rs.evaluate(&req("read_file", None, &["src/main.rs"]), RiskLevel::Safe),
            Action::Allow
        );
        assert_eq!(
            rs.evaluate(&req("write_file", None, &["out.txt"]), RiskLevel::Risky),
            Action::Ask
        );
    }

    #[test]
    fn sensitive_default_escalates_safe_read() {
        let rs = Ruleset::with_defaults();
        // A Safe read of .env is escalated to Ask.
        assert_eq!(
            rs.evaluate(&req("read_file", None, &[".env"]), RiskLevel::Safe),
            Action::Ask
        );
    }

    #[test]
    fn explicit_user_allow_beats_sensitive_default() {
        // B1 regression guard: user allow (tier 1) must win over the sensitive
        // default ask (tier 2).
        let rs = Ruleset::from_config(vec![rule("*", None, Some(".env"), RuleAction::Allow)]);
        assert_eq!(
            rs.evaluate(&req("read_file", None, &[".env"]), RiskLevel::Safe),
            Action::Allow
        );
        // And an explicit user deny still wins.
        let rs2 = Ruleset::from_config(vec![rule("*", None, Some(".env"), RuleAction::Deny)]);
        assert_eq!(
            rs2.evaluate(&req("read_file", None, &[".env"]), RiskLevel::Safe),
            Action::Deny
        );
    }

    // --- extract ---
    #[test]
    fn extract_pulls_command_and_path() {
        let r = extract("bash", &json!({"command": "ls -la"}));
        assert_eq!(r.bash_command.as_deref(), Some("ls -la"));
        assert!(r.paths.is_empty());

        let r = extract("edit", &json!({"path": "src/x.rs", "old": "a", "new": "b"}));
        assert_eq!(r.paths, vec!["src/x.rs".to_string()]);
        assert!(r.bash_command.is_none());
    }

    #[test]
    fn extract_never_fabricates_path_for_task_or_unknown() {
        let r = extract("task", &json!({"description": "do it", "path": "trap"}));
        assert!(r.paths.is_empty()); // `task` is not a file tool — path ignored
        assert!(r.bash_command.is_none());
        let r = extract("mcp__x__y", &json!({"path": "trap"}));
        assert!(r.paths.is_empty());
    }
}
