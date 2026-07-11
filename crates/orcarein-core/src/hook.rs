//! User-configured tool hooks (PreToolUse / PostToolUse).
//!
//! A [`HookSet`] runs user-declared command hooks around a tool call. Hooks are
//! matched by tool name, invoked as subprocesses with a JSON payload on stdin,
//! and interpreted by **exit code only** (v1): `0` = proceed, `2` = block
//! (PreToolUse), anything else = non-blocking error (the tool still runs).
//!
//! PreToolUse fires *before* the permission gate and can only tighten (block) —
//! never loosen. PostToolUse fires after a successful tool call and appends the
//! hook's stdout to the tool result as extra context (it cannot undo the call).
//!
//! Pure and silent (no tracing/println): errors travel back through the return
//! value, not stdout/stderr.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Per-hook timeout when `timeout` is omitted (seconds).
const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;
/// Cap on captured hook stdout/stderr fed back into the model context.
const MAX_HOOK_OUTPUT_BYTES: usize = 8 * 1024;

/// Which lifecycle point a hook fires at. Also the `hook_event_name` payload
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

impl HookEvent {
    fn as_str(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
        }
    }
}

/// One configured hook. `matcher` selects tools (`"bash"` / `"edit|write"` /
/// `"*"`); `command` is a shell command line; `timeout` is seconds (default 30).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEntry {
    pub matcher: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// The `[hooks]` config section. TOML keys are `PreToolUse` / `PostToolUse`
/// (Claude Code parity).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksConfig {
    #[serde(default, rename = "PreToolUse", skip_serializing_if = "Vec::is_empty")]
    pub pre_tool_use: Vec<HookEntry>,
    #[serde(default, rename = "PostToolUse", skip_serializing_if = "Vec::is_empty")]
    pub post_tool_use: Vec<HookEntry>,
}

/// Outcome of running the PreToolUse hooks for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// No hook blocked — proceed to the permission gate.
    Proceed,
    /// A hook exited 2 — block the tool. String = capped stderr reason.
    Block(String),
    /// A non-blocking hook error (spawn fail / timeout / other non-zero). The
    /// tool still runs; String = the note to surface (no `ERROR:` prefix).
    ProceedWithError(String),
}

/// Compiled hook set held by an `Agent`. Empty = no hooks (zero subprocess).
#[derive(Debug, Clone, Default)]
pub struct HookSet {
    pre: Vec<HookEntry>,
    post: Vec<HookEntry>,
}

/// Raw result of running one hook subprocess.
struct HookRun {
    /// Process exit code, or `None` when it was killed by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// Set on spawn failure / timeout (a non-blocking error).
    error: Option<String>,
}

impl HookRun {
    fn failed(msg: String) -> Self {
        HookRun {
            code: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(msg),
        }
    }
}

/// Does `matcher` select `tool_name`? `"*"` / empty = all; `"a|b"` = any
/// alternative equals the name; otherwise exact. No regex (zero-dep).
fn matcher_matches(matcher: &str, tool_name: &str) -> bool {
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    if matcher.contains('|') {
        return matcher.split('|').any(|m| m == tool_name);
    }
    matcher == tool_name
}

/// Expand a leading `~` / `~/` in the command's first token to the home dir.
fn expand_tilde(command: &str) -> String {
    if let Some(rest) = command.strip_prefix("~/") {
        if let Some(base) = directories::BaseDirs::new() {
            return format!("{}/{}", base.home_dir().to_string_lossy(), rest);
        }
    } else if command == "~" {
        if let Some(base) = directories::BaseDirs::new() {
            return base.home_dir().to_string_lossy().into_owned();
        }
    }
    command.to_string()
}

/// Build the JSON payload written to the hook's stdin.
fn build_payload(
    event: HookEvent,
    tool_name: &str,
    tool_input: &Value,
    tool_output: Option<&str>,
) -> String {
    let mut obj = json!({
        "hook_event_name": event.as_str(),
        "tool_name": tool_name,
        "tool_input": tool_input,
    });
    if let Ok(cwd) = std::env::current_dir() {
        obj["cwd"] = json!(cwd.to_string_lossy());
    }
    if let Some(out) = tool_output {
        obj["tool_output"] = json!(out);
    }
    obj.to_string()
}

/// Spawn one hook, feed the payload on stdin concurrently (avoids a pipe-buffer
/// deadlock on large `tool_input`), and collect its result under a timeout.
async fn run_one(
    entry: &HookEntry,
    event: HookEvent,
    tool_name: &str,
    tool_input: &Value,
    tool_output: Option<&str>,
) -> HookRun {
    let payload = build_payload(event, tool_name, tool_input, tool_output);
    let command = expand_tilde(&entry.command);
    let timeout = Duration::from_secs(entry.timeout.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS));
    let (program, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("bash", "-c")
    };

    let spawned = tokio::process::Command::new(program)
        .arg(flag)
        .arg(&command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true) // on timeout the dropped future kills the child
        .spawn();

    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return HookRun::failed(format!("{}: spawn failed: {e}", entry.command)),
    };

    // Write the payload from a detached task so we can drain stdout/stderr
    // concurrently via wait_with_output().
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = payload.into_bytes();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await; // EOF
        });
    }

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_elapsed) => HookRun::failed(format!("{}: timed out", entry.command)),
        Ok(Err(e)) => HookRun::failed(format!("{}: io error: {e}", entry.command)),
        Ok(Ok(output)) => HookRun {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            error: None,
        },
    }
}

impl HookSet {
    /// No hooks — every run is a no-op (never spawns a subprocess).
    pub fn empty() -> Self {
        HookSet::default()
    }

    /// Build from the user's `[hooks]` config.
    pub fn from_config(cfg: &HooksConfig) -> Self {
        HookSet {
            pre: cfg.pre_tool_use.clone(),
            post: cfg.post_tool_use.clone(),
        }
    }

    /// Are any PostToolUse hooks configured? (Lets the caller skip cloning the
    /// tool input when there is nothing to run.)
    pub fn has_post(&self) -> bool {
        !self.post.is_empty()
    }

    /// Run matching PreToolUse hooks in config order. The first `exit 2`
    /// short-circuits to `Block`. A non-blocking error is remembered but does
    /// not stop the scan (a later hook may still block).
    pub async fn run_pre(&self, tool_name: &str, tool_input: &Value) -> HookOutcome {
        let mut pending_error: Option<String> = None;
        for entry in self
            .pre
            .iter()
            .filter(|e| matcher_matches(&e.matcher, tool_name))
        {
            let r = run_one(entry, HookEvent::PreToolUse, tool_name, tool_input, None).await;
            if let Some(err) = r.error {
                pending_error.get_or_insert(err);
                continue;
            }
            match r.code {
                Some(0) => {}
                Some(2) => {
                    return HookOutcome::Block(crate::text::cap(&r.stderr, MAX_HOOK_OUTPUT_BYTES));
                }
                Some(other) => {
                    pending_error.get_or_insert(format!("{} exited {other}", entry.command));
                }
                None => {
                    pending_error.get_or_insert(format!("{} killed by signal", entry.command));
                }
            }
        }
        match pending_error {
            Some(msg) => HookOutcome::ProceedWithError(msg),
            None => HookOutcome::Proceed,
        }
    }

    /// Run matching PostToolUse hooks in config order. Returns the concatenated
    /// stdout context (`""` when nothing matched or all stdout was empty).
    /// Non-zero/errored hooks fold a `[hook error: …]` line into the string.
    /// Never blocks (PostToolUse cannot undo the call).
    pub async fn run_post(&self, tool_name: &str, tool_input: &Value, tool_output: &str) -> String {
        let mut ctx = String::new();
        for entry in self
            .post
            .iter()
            .filter(|e| matcher_matches(&e.matcher, tool_name))
        {
            let r = run_one(
                entry,
                HookEvent::PostToolUse,
                tool_name,
                tool_input,
                Some(tool_output),
            )
            .await;
            let piece = crate::text::cap(r.stdout.trim_end(), MAX_HOOK_OUTPUT_BYTES);
            if !piece.is_empty() {
                if !ctx.is_empty() {
                    ctx.push('\n');
                }
                ctx.push_str(&piece);
            }
            let err = r.error.or_else(|| match r.code {
                Some(0) => None,
                Some(other) => Some(format!("{} exited {other}", entry.command)),
                None => Some(format!("{} killed by signal", entry.command)),
            });
            if let Some(e) = err {
                if !ctx.is_empty() {
                    ctx.push('\n');
                }
                ctx.push_str(&format!("[hook error: {e}]"));
            }
        }
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(matcher: &str, command: &str) -> HookEntry {
        HookEntry {
            matcher: matcher.to_string(),
            command: command.to_string(),
            timeout: None,
        }
    }

    // --- matcher ---
    #[test]
    fn matcher_exact_alternation_and_wildcard() {
        assert!(matcher_matches("bash", "bash"));
        assert!(!matcher_matches("bash", "edit"));
        assert!(matcher_matches("edit|write", "edit"));
        assert!(matcher_matches("edit|write", "write"));
        assert!(!matcher_matches("edit|write", "bash"));
        assert!(matcher_matches("*", "anything"));
        assert!(matcher_matches("", "anything"));
    }

    // --- serde ---
    #[test]
    fn hooks_config_toml_roundtrips_with_cc_keys() {
        let src = "[[PreToolUse]]\nmatcher = \"bash\"\ncommand = \"guard.sh\"\n\
                   [[PostToolUse]]\nmatcher = \"edit|write\"\ncommand = \"fmt.sh\"\ntimeout = 5\n";
        let cfg: HooksConfig = toml::from_str(src).unwrap();
        assert_eq!(cfg.pre_tool_use.len(), 1);
        assert_eq!(cfg.pre_tool_use[0].matcher, "bash");
        assert_eq!(cfg.post_tool_use[0].timeout, Some(5));
        // Default serializes to nothing.
        assert_eq!(toml::to_string(&HooksConfig::default()).unwrap(), "");
    }

    // --- runner (real subprocess, portable inline commands) ---
    #[tokio::test]
    async fn pre_exit_2_blocks() {
        let hs = HookSet {
            pre: vec![entry("bash", "exit 2")],
            post: vec![],
        };
        let out = hs.run_pre("bash", &json!({"command": "ls"})).await;
        assert!(matches!(out, HookOutcome::Block(_)), "got {out:?}");
    }

    #[tokio::test]
    async fn pre_exit_0_proceeds() {
        let hs = HookSet {
            pre: vec![entry("bash", "exit 0")],
            post: vec![],
        };
        assert_eq!(
            hs.run_pre("bash", &json!({"command": "ls"})).await,
            HookOutcome::Proceed
        );
    }

    #[tokio::test]
    async fn pre_nonmatching_matcher_never_spawns_so_no_block() {
        // Property 1 (direct): the hook command is `exit 2`; if the matcher were
        // ignored and the command ran, this would Block. A Proceed proves the
        // subprocess was never spawned.
        let hs = HookSet {
            pre: vec![entry("other_tool", "exit 2")],
            post: vec![],
        };
        assert_eq!(
            hs.run_pre("bash", &json!({"command": "ls"})).await,
            HookOutcome::Proceed
        );
    }

    #[tokio::test]
    async fn pre_other_nonzero_is_nonblocking_error() {
        let hs = HookSet {
            pre: vec![entry("*", "exit 1")],
            post: vec![],
        };
        assert!(matches!(
            hs.run_pre("bash", &json!({})).await,
            HookOutcome::ProceedWithError(_)
        ));
    }

    #[tokio::test]
    async fn post_echo_returns_context() {
        let hs = HookSet {
            pre: vec![],
            post: vec![entry("edit", "echo ctx")],
        };
        let ctx = hs.run_post("edit", &json!({"path": "x"}), "done").await;
        assert!(ctx.contains("ctx"), "got {ctx:?}");
    }

    #[tokio::test]
    async fn post_nonmatching_returns_empty() {
        let hs = HookSet {
            pre: vec![],
            post: vec![entry("write", "echo ctx")],
        };
        assert_eq!(hs.run_post("edit", &json!({}), "done").await, "");
    }

    #[tokio::test]
    async fn pre_timeout_is_nonblocking_error() {
        // Portable sleep-past-timeout: 1s timeout, command sleeps ~3s.
        let sleep = if cfg!(windows) {
            "ping -n 4 127.0.0.1 >NUL"
        } else {
            "sleep 3"
        };
        let hs = HookSet {
            pre: vec![HookEntry {
                matcher: "*".into(),
                command: sleep.into(),
                timeout: Some(1),
            }],
            post: vec![],
        };
        assert!(matches!(
            hs.run_pre("bash", &json!({})).await,
            HookOutcome::ProceedWithError(_)
        ));
    }
}
