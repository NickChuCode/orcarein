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
    model: String,
    max_iterations: usize,
    persona: String,
}

impl SubagentTool {
    /// Builds a `task` tool. The factories are invoked once per `execute` so
    /// each child run gets its own registry + policy (no shared state).
    pub fn new(
        provider: Arc<dyn Provider>,
        registry_factory: Arc<dyn Fn() -> ToolRegistry + Send + Sync>,
        policy_factory: Arc<dyn Fn() -> Box<dyn PermissionPolicy> + Send + Sync>,
        model: String,
        max_iterations: usize,
        persona: String,
    ) -> Self {
        SubagentTool {
            provider,
            registry_factory,
            policy_factory,
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

        let registry = (self.registry_factory)();
        let defs = registry.definitions();
        let agent =
            Agent::new(&*self.provider, &registry, &defs).with_max_iterations(self.max_iterations);
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
    use anyhow::Result;
    use futures_util::stream::BoxStream;
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
}
