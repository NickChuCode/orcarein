//! `task` — the sub-agent tool.
//!
//! The model calls `task({"description": "..."})` and this tool runs a
//! **nested** [`Agent`] loop on a **fresh, isolated** [`Session`], returning
//! only the child's final text. The child's intermediate tool work stays in
//! the child session — that context isolation is the whole point: a large
//! search/exploration burns tokens inside the child, and the parent only sees
//! the distilled answer.
//!
//! Because [`Tool::execute`] cannot see the live [`PermissionPolicy`] / sink
//! (the parent borrows those mutably for its own turn), the pieces the child
//! needs — provider, tool registry, permission policy — are injected at
//! construction time as `Arc` factories. Each `execute` call mints a brand new
//! registry + policy so child runs never share state.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use super::{RiskLevel, Tool, ToolError, ToolOutput, ToolRegistry};
use crate::{Agent, AgentEvent, PermissionPolicy, Provider, Session};
use crate::{HookSet, PermissionMode, Ruleset, SharedMode};

/// Upper bound on the text returned to the parent (mirrors the bash tool's
/// cap) so a runaway child cannot blow up the parent's context window.
const OUTPUT_CAP: usize = 32 * 1024;

/// Default child system prompt (English — it enters the LLM prompt).
pub const DEFAULT_SUBAGENT_PERSONA: &str = "You are a sub-agent. Complete the \
delegated task autonomously, then return a concise, self-contained answer. You \
cannot see the parent conversation — everything you need is in the task \
description. Stop when done.";

/// Schema-validated arguments for `task`. A missing/non-string `description`
/// surfaces as [`ToolError::InvalidArguments`].
#[derive(Deserialize)]
struct TaskArgs {
    description: String,
}

/// The `task` tool — delegates a self-contained sub-task to a fresh sub-agent
/// with an isolated context window.
pub struct SubagentTool {
    provider: Arc<dyn Provider>,
    registry_factory: Arc<dyn Fn() -> ToolRegistry + Send + Sync>,
    policy_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync>,
    ruleset_factory: Arc<dyn Fn() -> Ruleset + Send + Sync>,
    mode: SharedMode,
    hooks: HookSet,
    model: String,
    max_iterations: usize,
    persona: String,
}

impl SubagentTool {
    /// Builds a `task` tool. The factories are invoked once per `execute` so
    /// each child run gets its own registry + policy (no shared state). The
    /// child inherits the parent's live permission mode (`mode`), permission
    /// ruleset (`ruleset_factory`), and hooks (`hooks`) — otherwise a `plan`
    /// mode session's `task(...)` call would be a jailbreak: the child would
    /// see every write tool and run under the un-restricted default ruleset.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn Provider>,
        registry_factory: Arc<dyn Fn() -> ToolRegistry + Send + Sync>,
        policy_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync>,
        ruleset_factory: Arc<dyn Fn() -> Ruleset + Send + Sync>,
        mode: SharedMode,
        hooks: HookSet,
        model: String,
        max_iterations: usize,
        persona: String,
    ) -> Self {
        SubagentTool {
            provider,
            registry_factory,
            policy_factory,
            ruleset_factory,
            mode,
            hooks,
            model,
            max_iterations,
            persona,
        }
    }
}

/// Marker appended when [`cap`] has to truncate.
const TRUNC_MARKER: &str = "\n[output truncated]";

/// Caps the returned string at [`OUTPUT_CAP`] bytes total. When truncation is
/// needed, headroom for [`TRUNC_MARKER`] is reserved first so the combined
/// length never exceeds the cap, and the content is cut on a char boundary.
fn cap(s: &str) -> String {
    if s.len() <= OUTPUT_CAP {
        return s.to_string();
    }
    // Reserve room for the marker so content + marker stays within the cap.
    let mut end = OUTPUT_CAP.saturating_sub(TRUNC_MARKER.len());
    // Walk back to the nearest char boundary at or below that budget.
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNC_MARKER}", &s[..end])
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate a self-contained sub-task to a fresh sub-agent with an \
isolated context window. It runs autonomously with its own tools and returns a \
concise result; its intermediate work does not enter this conversation. Use for \
large searches/explorations whose details you don't need to keep. Put \
everything it needs in `description`."
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string" }
            },
            "required": ["description"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // Must stay Safe: a Risky `task` would re-enter the parent's policy
        // borrow during dispatch. Child Risky tools are still gated by the
        // child's own policy.
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let TaskArgs { description } = serde_json::from_value(args)?;

        // Fresh, isolated child session — it cannot see the parent history.
        let mut child_session = Session::new(&self.persona);
        child_session.push_user(&description);

        let m: PermissionMode = self.mode.get();
        let registry = (self.registry_factory)();
        let defs = m.filter_defs(registry.definitions());
        let agent = Agent::new(&*self.provider, &registry, &defs)
            .with_max_iterations(self.max_iterations)
            .with_ruleset((self.ruleset_factory)().with_mode(m))
            .with_hooks(self.hooks.clone());
        let mut policy = (self.policy_factory)();
        let mut quiet = |_ev: AgentEvent| {}; // events stay in the child

        let content = match agent
            .run_turn(&mut child_session, &self.model, &mut *policy, &mut quiet)
            .await
        {
            Ok(outcome) => {
                let mut c = outcome.content;
                if outcome.hit_iteration_limit {
                    c.push_str(&format!(
                        "\n[subagent did not converge within {} steps]",
                        self.max_iterations
                    ));
                }
                c
            }
            Err(e) => format!("ERROR: subagent failed: {e}. Try a different decomposition."),
        };

        Ok(ToolOutput::new(cap(&content)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowlistPolicy, ChatOptions, MockProvider, StreamEvent, ToolOutput};
    use crate::{HookEntry, HooksConfig, PermissionRule, RuleAction};
    use anyhow::Result;
    use futures_util::stream::{self, BoxStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A `Safe` tool that echoes its raw arguments back as the result.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo arguments back"
        }
        fn schema(&self) -> serde_json::Value {
            json!({ "type": "object" })
        }
        fn risk_level(&self) -> RiskLevel {
            RiskLevel::Safe
        }
        async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(args.to_string()))
        }
    }

    /// A `Provider` whose `chat_stream` always errors — to drive the
    /// failure-mapping path.
    struct FailProvider;

    #[async_trait]
    impl Provider for FailProvider {
        fn name(&self) -> &str {
            "fail"
        }
        fn default_model(&self) -> &str {
            "m"
        }
        async fn chat_stream(
            &self,
            _messages: &[crate::Message],
            _tools: &[crate::ToolDefinition],
            _opts: &ChatOptions,
        ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
            Err(anyhow::anyhow!("boom"))
        }
    }

    fn deny_all_factory() -> Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync> {
        Arc::new(|| Box::new(AllowlistPolicy::deny_all()))
    }

    #[tokio::test]
    async fn execute_runs_a_multistep_child_and_returns_its_final_text() {
        let child_provider = Arc::new(MockProvider::new());
        child_provider.push_tool_call("c", "echo", "{}");
        child_provider.push_text("CHILD RESULT");
        let probe = child_provider.clone();

        let tool = SubagentTool::new(
            child_provider,
            Arc::new(|| {
                let mut r = ToolRegistry::new();
                r.register(Box::new(EchoTool));
                r
            }),
            deny_all_factory(),
            Arc::new(|| Ruleset::with_defaults()),
            SharedMode::new(PermissionMode::Default),
            HookSet::empty(),
            "m".into(),
            8,
            DEFAULT_SUBAGENT_PERSONA.into(),
        );

        let out = tool
            .execute(json!({ "description": "do x" }))
            .await
            .unwrap();
        assert_eq!(out.content, "CHILD RESULT");
        // Both scripted steps were consumed => the child ran multi-step.
        assert_eq!(probe.pending(), 0);
    }

    #[tokio::test]
    async fn execute_rejects_missing_description() {
        let provider = Arc::new(MockProvider::new());
        let tool = SubagentTool::new(
            provider,
            Arc::new(ToolRegistry::new),
            deny_all_factory(),
            Arc::new(|| Ruleset::with_defaults()),
            SharedMode::new(PermissionMode::Default),
            HookSet::empty(),
            "m".into(),
            8,
            DEFAULT_SUBAGENT_PERSONA.into(),
        );

        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_maps_child_failure_to_error_string() {
        let tool = SubagentTool::new(
            Arc::new(FailProvider),
            Arc::new(ToolRegistry::new),
            deny_all_factory(),
            Arc::new(|| Ruleset::with_defaults()),
            SharedMode::new(PermissionMode::Default),
            HookSet::empty(),
            "m".into(),
            8,
            DEFAULT_SUBAGENT_PERSONA.into(),
        );

        let out = tool.execute(json!({ "description": "x" })).await.unwrap();
        assert!(
            out.content.starts_with("ERROR:"),
            "expected ERROR-prefixed content, got: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn parent_session_sees_only_the_result_not_child_intermediate() {
        // Child distills CHILD_INTERMEDIATE down to DISTILLED RESULT.
        let child_provider = Arc::new(MockProvider::new());
        child_provider.push_tool_call("c", "echo", r#"{"secret":"CHILD_INTERMEDIATE"}"#);
        child_provider.push_text("DISTILLED RESULT");

        // Sanity: the factory-built child registry has no `task` (no recursion).
        let child_factory: Arc<dyn Fn() -> ToolRegistry + Send + Sync> = Arc::new(|| {
            let mut r = ToolRegistry::new();
            r.register(Box::new(EchoTool));
            r
        });
        assert!(!child_factory().names().contains(&"task"));

        let task_tool = SubagentTool::new(
            child_provider,
            child_factory,
            deny_all_factory(),
            Arc::new(|| Ruleset::with_defaults()),
            SharedMode::new(PermissionMode::Default),
            HookSet::empty(),
            "m".into(),
            8,
            DEFAULT_SUBAGENT_PERSONA.into(),
        );

        // Parent registry exposes the `task` tool.
        let mut parent_registry = ToolRegistry::new();
        parent_registry.register(Box::new(task_tool));
        let parent_defs = parent_registry.definitions();

        // Parent provider: call task, then finish.
        let parent_provider = MockProvider::new();
        parent_provider.push_tool_call("t1", "task", r#"{"description":"explore"}"#);
        parent_provider.push_text("PARENT DONE");

        let agent = Agent::new(&parent_provider, &parent_registry, &parent_defs);
        let mut parent_session = Session::new("parent sys");
        parent_session.push_user("go");
        let mut policy = AllowlistPolicy::deny_all();
        let mut sink = |_e: AgentEvent| {};

        agent
            .run_turn(&mut parent_session, "m", &mut policy, &mut sink)
            .await
            .unwrap();

        // (a) the distilled result reached the parent as a tool message.
        assert!(parent_session
            .messages()
            .iter()
            .any(|m| m.role == "tool" && m.content.contains("DISTILLED RESULT")));

        // (b) the child's intermediate work never leaked into the parent.
        assert!(
            !parent_session
                .messages()
                .iter()
                .any(|m| m.content.contains("CHILD_INTERMEDIATE")),
            "child intermediate must not leak into the parent session"
        );
    }

    #[tokio::test]
    async fn execute_appends_did_not_converge_on_iteration_limit() {
        // max_iterations = 1, but the child keeps calling a tool, so the loop
        // hits the cap before the model ever produces a final text answer.
        let child_provider = Arc::new(MockProvider::new());
        child_provider.push_tool_call("c1", "echo", "{}");
        child_provider.push_tool_call("c2", "echo", "{}");
        child_provider.push_tool_call("c3", "echo", "{}");

        let tool = SubagentTool::new(
            child_provider,
            Arc::new(|| {
                let mut r = ToolRegistry::new();
                r.register(Box::new(EchoTool));
                r
            }),
            deny_all_factory(),
            Arc::new(|| Ruleset::with_defaults()),
            SharedMode::new(PermissionMode::Default),
            HookSet::empty(),
            "m".into(),
            1,
            DEFAULT_SUBAGENT_PERSONA.into(),
        );

        let out = tool
            .execute(json!({ "description": "spin" }))
            .await
            .unwrap();
        assert!(
            out.content.contains("did not converge"),
            "expected non-convergence note, got: {:?}",
            out.content
        );
    }

    #[test]
    fn cap_truncates_multibyte_without_panic_and_stays_within_cap() {
        // "你" is 3 bytes; repeating it past the cap guarantees the byte budget
        // lands mid-codepoint, exercising the char-boundary walk-back.
        let s = "你".repeat(OUTPUT_CAP); // ~3x OUTPUT_CAP bytes
        let out = cap(&s);

        // No panic + valid UTF-8 (a String is UTF-8 by construction; the cut
        // must land on a boundary or `&s[..end]` would have panicked above).
        assert!(out.len() <= OUTPUT_CAP, "total must stay within the cap");
        assert!(
            out.ends_with(TRUNC_MARKER),
            "must carry the truncation marker"
        );
    }

    #[test]
    fn cap_leaves_short_input_unchanged() {
        let s = "short result";
        let out = cap(s);
        assert_eq!(out, s);
        assert!(!out.ends_with(TRUNC_MARKER));
    }

    // --- ruleset/mode/hooks inheritance (jailbreak closure) ---

    /// A `Provider` double for exercising exactly one child tool call: its
    /// first response calls `tool_name`; every later response echoes back the
    /// content of the most recent `tool`-role message as final text.
    ///
    /// `TurnOutcome::content` (what `SubagentTool::execute` returns to its
    /// caller) is only ever the model's *final* turn text — a mid-turn tool
    /// result never resurfaces there on its own (see
    /// `parent_session_sees_only_the_result_not_child_intermediate` above,
    /// which relies on exactly that isolation). This double simulates a
    /// model that relays what it was just told, so a denied/blocked/executed
    /// tool call becomes observable through the one channel the public
    /// `execute()` API exposes.
    struct EchoOneCall {
        tool_name: &'static str,
        args: &'static str,
    }

    #[async_trait]
    impl Provider for EchoOneCall {
        fn name(&self) -> &str {
            "echo-one-call"
        }
        fn default_model(&self) -> &str {
            "m"
        }
        async fn chat_stream(
            &self,
            messages: &[crate::Message],
            _tools: &[crate::ToolDefinition],
            _opts: &ChatOptions,
        ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
            let events = match messages.iter().rev().find(|m| m.role == "tool") {
                Some(tool_msg) => vec![StreamEvent::Content(tool_msg.content.clone())],
                None => vec![StreamEvent::ToolCalls(vec![crate::ToolCall {
                    id: "c".into(),
                    kind: "function".into(),
                    function: crate::FunctionCall {
                        name: self.tool_name.into(),
                        arguments: self.args.into(),
                    },
                }])],
            };
            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
        }
    }

    #[tokio::test]
    async fn subagent_inherits_parent_deny_rule() {
        // Parent denies bash -> child must be denied bash too. Today the
        // child gets `Ruleset::with_defaults()` and would run it: this test
        // fails on that code path (bash executes, `ran` > 0) and passes once
        // `execute()` builds the child ruleset from `ruleset_factory` +
        // `mode` instead.
        let ran = Arc::new(AtomicUsize::new(0));

        struct RiskyBash(Arc<AtomicUsize>);
        #[async_trait]
        impl Tool for RiskyBash {
            fn name(&self) -> &str {
                "bash"
            }
            fn description(&self) -> &str {
                "run a shell command"
            }
            fn schema(&self) -> serde_json::Value {
                json!({ "type": "object" })
            }
            fn risk_level(&self) -> RiskLevel {
                RiskLevel::Risky
            }
            async fn execute(&self, _args: serde_json::Value) -> Result<ToolOutput, ToolError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::new("BASH RAN"))
            }
        }

        let probe = ran.clone();
        let child_registry_factory: Arc<dyn Fn() -> ToolRegistry + Send + Sync> =
            Arc::new(move || {
                let mut r = ToolRegistry::new();
                r.register(Box::new(RiskyBash(probe.clone())));
                r
            });
        let allow_all_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync> =
            Arc::new(|| Box::new(AllowlistPolicy::allow_all()));
        let deny_bash_factory: Arc<dyn Fn() -> Ruleset + Send + Sync> = Arc::new(|| {
            Ruleset::from_config(vec![PermissionRule {
                tool: "bash".into(),
                command: None,
                path: None,
                action: RuleAction::Deny,
            }])
        });

        let tool = SubagentTool::new(
            Arc::new(EchoOneCall {
                tool_name: "bash",
                args: "{}",
            }),
            child_registry_factory,
            allow_all_factory,
            deny_bash_factory,
            SharedMode::new(PermissionMode::Default),
            HookSet::empty(),
            "m".into(),
            8,
            DEFAULT_SUBAGENT_PERSONA.into(),
        );

        let out = tool
            .execute(json!({ "description": "try bash" }))
            .await
            .unwrap();

        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "bash must never execute once the parent's deny rule is inherited"
        );
        assert!(
            out.content.contains("permission denied"),
            "got: {}",
            out.content
        );
    }

    #[test]
    fn subagent_child_defs_hide_write_tools_in_plan() {
        // With mode=Plan, the defs the child advertises exclude write tools —
        // the same ceiling `PermissionMode::filter_defs` applies to the
        // parent's own tool list (spec 2026-07-13 §3.1).
        struct ReadFileMock;
        #[async_trait]
        impl Tool for ReadFileMock {
            fn name(&self) -> &str {
                "read_file"
            }
            fn description(&self) -> &str {
                "read a file"
            }
            fn schema(&self) -> serde_json::Value {
                json!({ "type": "object" })
            }
            fn risk_level(&self) -> RiskLevel {
                RiskLevel::Safe
            }
            async fn execute(&self, _args: serde_json::Value) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::new("contents"))
            }
        }
        struct BashMock;
        #[async_trait]
        impl Tool for BashMock {
            fn name(&self) -> &str {
                "bash"
            }
            fn description(&self) -> &str {
                "run a shell command"
            }
            fn schema(&self) -> serde_json::Value {
                json!({ "type": "object" })
            }
            fn risk_level(&self) -> RiskLevel {
                RiskLevel::Risky
            }
            async fn execute(&self, _args: serde_json::Value) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::new("ran"))
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ReadFileMock));
        registry.register(Box::new(BashMock));

        let mode = SharedMode::new(PermissionMode::Plan);
        let defs = mode.get().filter_defs(registry.definitions());
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();

        assert!(
            names.contains(&"read_file"),
            "plan must keep read-only tools: {names:?}"
        );
        assert!(!names.contains(&"bash"), "plan must hide bash: {names:?}");
    }

    #[tokio::test]
    async fn subagent_inherits_parent_prehook_block() {
        // Mirrors agent.rs's `pretooluse_block_fires_before_permission_gate`:
        // a PreToolUse hook that exits 2 blocks the child's tool before the
        // permission gate ever runs, even though the ruleset/policy here
        // would otherwise allow it outright.
        struct FakeSafe;
        #[async_trait]
        impl Tool for FakeSafe {
            fn name(&self) -> &str {
                "faketool"
            }
            fn description(&self) -> &str {
                "fake"
            }
            fn schema(&self) -> serde_json::Value {
                json!({ "type": "object" })
            }
            fn risk_level(&self) -> RiskLevel {
                RiskLevel::Safe
            }
            async fn execute(&self, _args: serde_json::Value) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::new("TOOL_RAN"))
            }
        }

        let child_registry_factory: Arc<dyn Fn() -> ToolRegistry + Send + Sync> = Arc::new(|| {
            let mut r = ToolRegistry::new();
            r.register(Box::new(FakeSafe));
            r
        });
        let allow_all_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync> =
            Arc::new(|| Box::new(AllowlistPolicy::allow_all()));
        let hooks = HookSet::from_config(&HooksConfig {
            pre_tool_use: vec![HookEntry {
                matcher: "faketool".into(),
                command: "exit 2".into(),
                timeout: None,
            }],
            post_tool_use: vec![],
        });

        let tool = SubagentTool::new(
            Arc::new(EchoOneCall {
                tool_name: "faketool",
                args: "{}",
            }),
            child_registry_factory,
            allow_all_factory,
            Arc::new(|| Ruleset::with_defaults()),
            SharedMode::new(PermissionMode::Default),
            hooks,
            "m".into(),
            8,
            DEFAULT_SUBAGENT_PERSONA.into(),
        );

        let out = tool
            .execute(json!({ "description": "try faketool" }))
            .await
            .unwrap();

        assert!(
            out.content.contains("blocked by PreToolUse hook"),
            "got: {}",
            out.content
        );
    }
}
