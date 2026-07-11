//! Health-check diagnostics behind `orcarein doctor`.
//!
//! The verdict logic lives here as small **pure** functions: each takes
//! already-gathered facts (does the file exist? is the key present?) and
//! returns a [`Check`]. The binary does the real I/O — reading files,
//! probing env vars, stat-ing directories — then calls these so every
//! verdict is unit-testable offline, without touching the filesystem.
//!
//! `doctor` is intentionally **offline**: it never contacts a provider's
//! API (no network, no token spend). It reports whether OrcaRein is
//! *configured* to run, not whether the remote endpoint is reachable.

use std::fmt;

/// Severity of a single health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Everything is in order.
    Pass,
    /// Usable, but something is missing or sub-optimal (e.g. no API key yet).
    Warn,
    /// Broken — OrcaRein cannot function until this is fixed.
    Fail,
}

impl CheckStatus {
    /// Fixed-width label for the report table.
    pub fn label(self) -> &'static str {
        match self {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        }
    }
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One health-check result: a short name, a verdict, and a human-readable
/// detail line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

impl Check {
    pub fn new(name: impl Into<String>, status: CheckStatus, detail: impl Into<String>) -> Self {
        Check {
            name: name.into(),
            status,
            detail: detail.into(),
        }
    }

    pub fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Check::new(name, CheckStatus::Pass, detail)
    }

    pub fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Check::new(name, CheckStatus::Warn, detail)
    }

    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Check::new(name, CheckStatus::Fail, detail)
    }
}

/// `orcarein <version> on <os>/<arch>` — always informational (PASS).
pub fn build_info(version: &str, os: &str, arch: &str) -> Check {
    Check::pass("build", format!("orcarein {version} on {os}/{arch}"))
}

/// `config.toml`: missing-dir → WARN, malformed → FAIL, absent-file → PASS
/// (built-in defaults apply), present-and-parsed → PASS.
pub fn config_check(located: bool, path: &str, exists: bool, parse_error: Option<&str>) -> Check {
    if !located {
        return Check::warn(
            "config",
            "could not locate a config directory for this platform",
        );
    }
    if let Some(e) = parse_error {
        return Check::fail("config", format!("malformed {path}: {e}"));
    }
    if !exists {
        return Check::pass(
            "config",
            format!("no config.toml yet — using built-in defaults ({path})"),
        );
    }
    Check::pass("config", format!("OK ({path})"))
}

/// `secrets.toml`: missing-dir → WARN, malformed → FAIL, absent-file → PASS
/// (keys come from the environment), present → PASS unless the Unix mode is
/// not `0600` (world/group-readable secrets → WARN).
///
/// `mode_0600` is `Some(true/false)` on Unix and `None` elsewhere (Windows
/// has no POSIX mode; we rely on the user profile directory's ACL).
pub fn secrets_check(
    located: bool,
    path: &str,
    exists: bool,
    parse_error: Option<&str>,
    mode_0600: Option<bool>,
) -> Check {
    if !located {
        return Check::warn(
            "secrets",
            "could not locate a config directory for this platform",
        );
    }
    if let Some(e) = parse_error {
        return Check::fail("secrets", format!("malformed {path}: {e}"));
    }
    if !exists {
        return Check::pass(
            "secrets",
            format!("no secrets.toml — keys come from env vars ({path})"),
        );
    }
    if mode_0600 == Some(false) {
        return Check::warn(
            "secrets",
            format!("{path} is not mode 0600 — other users may be able to read your API keys"),
        );
    }
    Check::pass("secrets", format!("OK ({path})"))
}

/// Resolved provider name: a known backend → PASS, anything else → FAIL.
pub fn provider_check(name: &str, known: bool) -> Check {
    if known {
        Check::pass("provider", format!("'{name}'"))
    } else {
        Check::fail(
            "provider",
            format!("unknown provider '{name}' (expected: deepseek | openai)"),
        )
    }
}

/// API key for the selected provider. Present → PASS (noting the source);
/// absent → WARN (it's a setup state, not a broken one — you may be running
/// `doctor` before configuring a key).
pub fn api_key_check(
    provider: &str,
    key_present: bool,
    source: Option<&str>,
    env_var: &str,
) -> Check {
    if key_present {
        let src = source.unwrap_or("unknown source");
        Check::pass("api_key", format!("found for '{provider}' (via {src})"))
    } else {
        Check::warn(
            "api_key",
            format!(
                "no API key for '{provider}' — run `orcarein login` to store one, or set {env_var}"
            ),
        )
    }
}

/// Session storage directory: missing-dir → WARN, not writable → FAIL,
/// otherwise PASS with the saved-session count.
pub fn data_dir_check(
    located: bool,
    path: &str,
    writable: bool,
    session_count: Option<usize>,
) -> Check {
    if !located {
        return Check::warn(
            "data_dir",
            "could not locate a data directory for this platform",
        );
    }
    if !writable {
        return Check::fail("data_dir", format!("not writable: {path}"));
    }
    let n = session_count.unwrap_or(0);
    Check::pass("data_dir", format!("{n} saved session(s) ({path})"))
}

/// Registered tools: none → WARN (the model can still chat, but can't act),
/// otherwise PASS listing them.
pub fn tools_check(tools: &[&str]) -> Check {
    if tools.is_empty() {
        Check::warn(
            "tools",
            "no tools registered — the model cannot read or modify files",
        )
    } else {
        Check::pass(
            "tools",
            format!("{} registered: {}", tools.len(), tools.join(", ")),
        )
    }
}

/// Tally of how many checks landed in each bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
}

/// Counts the checks by status.
pub fn tally(checks: &[Check]) -> Tally {
    let mut t = Tally::default();
    for c in checks {
        match c.status {
            CheckStatus::Pass => t.pass += 1,
            CheckStatus::Warn => t.warn += 1,
            CheckStatus::Fail => t.fail += 1,
        }
    }
    t
}

/// The most severe status across all checks (`Fail` > `Warn` > `Pass`).
/// An empty slice is treated as `Pass`. The binary maps `Fail` to a non-zero
/// exit code so scripts / CI can gate on it.
pub fn worst_status(checks: &[Check]) -> CheckStatus {
    let mut worst = CheckStatus::Pass;
    for c in checks {
        match c.status {
            CheckStatus::Fail => return CheckStatus::Fail,
            CheckStatus::Warn => worst = CheckStatus::Warn,
            CheckStatus::Pass => {}
        }
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_are_fixed_width() {
        assert_eq!(CheckStatus::Pass.label(), "PASS");
        assert_eq!(CheckStatus::Warn.label(), "WARN");
        assert_eq!(CheckStatus::Fail.label(), "FAIL");
    }

    #[test]
    fn config_missing_dir_warns() {
        assert_eq!(
            config_check(false, "", false, None).status,
            CheckStatus::Warn
        );
    }

    #[test]
    fn config_malformed_fails() {
        let c = config_check(true, "/cfg.toml", true, Some("expected `=`"));
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("malformed"));
    }

    #[test]
    fn config_absent_is_pass_with_defaults() {
        let c = config_check(true, "/cfg.toml", false, None);
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("built-in defaults"));
    }

    #[test]
    fn config_present_is_pass() {
        assert_eq!(
            config_check(true, "/cfg.toml", true, None).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn secrets_loose_permissions_warn() {
        let c = secrets_check(true, "/s.toml", true, None, Some(false));
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("0600"));
    }

    #[test]
    fn secrets_tight_or_non_unix_pass() {
        assert_eq!(
            secrets_check(true, "/s.toml", true, None, Some(true)).status,
            CheckStatus::Pass
        );
        assert_eq!(
            secrets_check(true, "/s.toml", true, None, None).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn secrets_absent_is_pass() {
        assert_eq!(
            secrets_check(true, "/s.toml", false, None, None).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn provider_known_vs_unknown() {
        assert_eq!(provider_check("deepseek", true).status, CheckStatus::Pass);
        let c = provider_check("anthropic", false);
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("anthropic"));
    }

    #[test]
    fn api_key_present_passes_absent_warns() {
        assert_eq!(
            api_key_check(
                "deepseek",
                true,
                Some("env DEEPSEEK_API_KEY"),
                "DEEPSEEK_API_KEY"
            )
            .status,
            CheckStatus::Pass
        );
        let c = api_key_check("deepseek", false, None, "DEEPSEEK_API_KEY");
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.contains("DEEPSEEK_API_KEY"));
        assert!(c.detail.contains("orcarein login"));
    }

    #[test]
    fn data_dir_not_writable_fails() {
        assert_eq!(
            data_dir_check(true, "/d", false, None).status,
            CheckStatus::Fail
        );
        let c = data_dir_check(true, "/d", true, Some(3));
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains('3'));
    }

    #[test]
    fn tools_empty_warns() {
        assert_eq!(tools_check(&[]).status, CheckStatus::Warn);
        let c = tools_check(&["read_file", "bash"]);
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.contains("read_file"));
    }

    #[test]
    fn worst_status_picks_most_severe() {
        assert_eq!(worst_status(&[]), CheckStatus::Pass);
        assert_eq!(worst_status(&[Check::pass("a", "")]), CheckStatus::Pass);
        assert_eq!(
            worst_status(&[Check::pass("a", ""), Check::warn("b", "")]),
            CheckStatus::Warn
        );
        assert_eq!(
            worst_status(&[
                Check::pass("a", ""),
                Check::warn("b", ""),
                Check::fail("c", "")
            ]),
            CheckStatus::Fail
        );
    }

    #[test]
    fn tally_counts_each_bucket() {
        let checks = vec![
            Check::pass("a", ""),
            Check::pass("b", ""),
            Check::warn("c", ""),
            Check::fail("d", ""),
        ];
        let t = tally(&checks);
        assert_eq!((t.pass, t.warn, t.fail), (2, 1, 1));
    }
}
