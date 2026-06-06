//! The headless agent loop — extracted from the binary so it can drive both
//! the interactive REPL and the non-interactive `orcarein run` mode, and be
//! embedded directly by downstream consumers (the future issue bot).
//!
//! This is the first **library-first** slice: the binary owns I/O (printing,
//! prompting, stdin), the core owns the *loop*. Two seams make that split:
//!
//! - [`EventSink`] — where a frontend receives [`AgentEvent`]s. The REPL
//!   prints them; `run` writes content to stdout and the rest to stderr; an
//!   embedder might collect them into a buffer.
//! - [`PermissionPolicy`] — how a frontend decides whether a `Risky` tool may
//!   run. The REPL prompts the human; `run` consults an [`AllowlistPolicy`];
//!   a future sandbox is just another implementation.
//!
//! Tool failures and permission denials are **not** errors: their message is
//! fed back to the model as the tool result so it can react. Only a
//! provider/transport failure aborts the turn ([`AgentError`]).

use std::collections::HashSet;

use futures_util::StreamExt;

use crate::{
    ChatOptions, Decision, Message, Provider, RiskLevel, Session, StreamEvent, TokenUsage,
    ToolCall, ToolDefinition, ToolRegistry,
};

/// Upper bound on tool-call iterations within one user turn. Prevents an
/// over-eager model from spinning the dispatcher forever.
pub const MAX_TOOL_ITERATIONS: usize = 8;

/// Prefix every synthetic tool-failure / permission-denial result string
/// carries, so a frontend (or the model) can recognise an unsuccessful call.
const ERROR_PREFIX: &str = "ERROR:";

/// A high-level event from one agent turn.
///
/// Higher level than the provider's [`StreamEvent`]: it adds explicit tool
/// start/finish so a frontend can render or log tool activity without
/// re-deriving it from the raw stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// A chunk of the model's reasoning ("thinking") trace.
    Reasoning(String),
    /// A chunk of the model's user-facing reply.
    Content(String),
    /// A tool is about to run (emitted before the permission check).
    ToolStarted {
        id: String,
        name: String,
        arguments: String,
    },
    /// A tool finished. `is_error` is true for a denied or failed call; the
    /// `result` string is fed back to the model either way.
    ToolFinished {
        id: String,
        name: String,
        result: String,
        is_error: bool,
    },
    /// The model reported token usage for one request.
    Usage(TokenUsage),
    /// The turn stopped because it hit [`MAX_TOOL_ITERATIONS`].
    IterationLimit,
}

/// Where a frontend receives [`AgentEvent`]s as the turn runs.
///
/// Any `FnMut(AgentEvent)` is also an `EventSink` (blanket impl below), so a
/// test or quick embedder can pass a closure.
pub trait EventSink {
    /// Receive one event. Called synchronously as the turn progresses.
    fn emit(&mut self, event: AgentEvent);
}

impl<F: FnMut(AgentEvent)> EventSink for F {
    fn emit(&mut self, event: AgentEvent) {
        self(event)
    }
}

/// Decides whether a `Risky` tool may run.
///
/// Only consulted for [`RiskLevel::Risky`] tools — `Safe` tools always run.
/// The REPL implements this by prompting the human; `run` mode by consulting
/// an [`AllowlistPolicy`].
pub trait PermissionPolicy {
    /// Return a [`Decision`] for running `tool` (with raw `args`) at `risk`.
    fn decide(&mut self, tool: &str, args: &str, risk: RiskLevel) -> Decision;
}

/// Permission policy for non-interactive runs: **deny every `Risky` tool**
/// unless it is on the allowlist (or `allow_all` is set — the explicit
/// escape hatch behind `--no-permission`).
///
/// This is the secure-by-default posture for `orcarein run`, the opposite of
/// the old blunt "skip all gating" flag.
#[derive(Debug, Clone, Default)]
pub struct AllowlistPolicy {
    allowed: HashSet<String>,
    allow_all: bool,
}

impl AllowlistPolicy {
    /// Deny every `Risky` tool (the safe default for `orcarein run`).
    pub fn deny_all() -> Self {
        AllowlistPolicy {
            allowed: HashSet::new(),
            allow_all: false,
        }
    }

    /// Allow exactly the named tools; deny all other `Risky` ones.
    pub fn from_allowed<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        AllowlistPolicy {
            allowed: names.into_iter().map(Into::into).collect(),
            allow_all: false,
        }
    }

    /// Allow every tool — the `--no-permission` escape hatch. The caller is
    /// responsible for warning the user that gating is off.
    pub fn allow_all() -> Self {
        AllowlistPolicy {
            allowed: HashSet::new(),
            allow_all: true,
        }
    }
}

impl PermissionPolicy for AllowlistPolicy {
    fn decide(&mut self, tool: &str, _args: &str, _risk: RiskLevel) -> Decision {
        if self.allow_all || self.allowed.contains(tool) {
            Decision::AllowOnce
        } else {
            Decision::DenyOnce
        }
    }
}

/// What one completed turn produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnOutcome {
    /// The model's final user-facing text (the last assistant reply).
    pub content: String,
    /// Token usage summed across every request made in the turn.
    pub usage: TokenUsage,
    /// True if the turn stopped because it hit [`MAX_TOOL_ITERATIONS`] rather
    /// than the model finishing on its own.
    pub hit_iteration_limit: bool,
}

/// Errors that abort an agent turn.
///
/// Note: tool failures and permission denials are deliberately **not** here —
/// they become tool-result messages fed back to the model. Only a
/// provider/transport failure surfaces as an `AgentError`.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The provider failed to start or mid-stream (network, HTTP, decode).
    #[error(transparent)]
    Provider(#[from] anyhow::Error),
}

/// The agent loop. Holds references to the pieces it needs; [`run_turn`]
/// drives one user turn to completion.
///
/// [`run_turn`]: Agent::run_turn
pub struct Agent<'a> {
    provider: &'a dyn Provider,
    registry: &'a ToolRegistry,
    tool_defs: &'a [ToolDefinition],
}

impl<'a> Agent<'a> {
    /// Builds an agent over the given provider, tool registry, and the tool
    /// definitions to advertise to the model (usually `registry.definitions()`).
    pub fn new(
        provider: &'a dyn Provider,
        registry: &'a ToolRegistry,
        tool_defs: &'a [ToolDefinition],
    ) -> Self {
        Agent {
            provider,
            registry,
            tool_defs,
        }
    }

    /// Runs one user turn to completion: streams a completion, executes any
    /// tool calls (gating `Risky` ones through `policy`), feeds the results
    /// back, and repeats until the model stops calling tools or
    /// [`MAX_TOOL_ITERATIONS`] is reached.
    ///
    /// The caller must have already pushed the user's message onto `session`.
    /// Events stream through `sink`; the final text and usage come back in the
    /// [`TurnOutcome`].
    pub async fn run_turn(
        &self,
        session: &mut Session,
        model: &str,
        policy: &mut dyn PermissionPolicy,
        sink: &mut dyn EventSink,
    ) -> Result<TurnOutcome, AgentError> {
        let opts = ChatOptions::new(model);
        let mut turn_usage = TokenUsage::default();
        let mut final_content = String::new();
        let mut iteration = 0usize;

        loop {
            if iteration >= MAX_TOOL_ITERATIONS {
                sink.emit(AgentEvent::IterationLimit);
                session.record_usage(turn_usage);
                return Ok(TurnOutcome {
                    content: final_content,
                    usage: turn_usage,
                    hit_iteration_limit: true,
                });
            }

            let mut stream = self
                .provider
                .chat_stream(session.messages(), self.tool_defs, &opts)
                .await?;

            let mut content = String::new();
            let mut reasoning = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();

            while let Some(event) = stream.next().await {
                match event? {
                    StreamEvent::Reasoning(text) => {
                        reasoning.push_str(&text);
                        sink.emit(AgentEvent::Reasoning(text));
                    }
                    StreamEvent::Content(text) => {
                        content.push_str(&text);
                        sink.emit(AgentEvent::Content(text));
                    }
                    StreamEvent::ToolCalls(calls) => tool_calls = calls,
                    StreamEvent::Usage(u) => {
                        turn_usage.add(u);
                        sink.emit(AgentEvent::Usage(u));
                    }
                }
            }

            let assistant_msg = if tool_calls.is_empty() {
                Message::assistant(&content).with_reasoning(reasoning)
            } else {
                Message::assistant_with_tool_calls(&content, tool_calls.clone())
                    .with_reasoning(reasoning)
            };
            session.push_assistant(assistant_msg);

            if tool_calls.is_empty() {
                final_content = content;
                break;
            }

            for call in &tool_calls {
                let result = self.dispatch(policy, call, sink).await;
                session.push_assistant(Message::tool(&call.id, result));
            }

            iteration += 1;
        }

        session.record_usage(turn_usage);
        Ok(TurnOutcome {
            content: final_content,
            usage: turn_usage,
            hit_iteration_limit: false,
        })
    }

    /// Emits start/finish events around a single tool call and returns the
    /// string fed back to the model (tool output, or an `ERROR:`-prefixed
    /// message on denial/failure).
    async fn dispatch(
        &self,
        policy: &mut dyn PermissionPolicy,
        call: &ToolCall,
        sink: &mut dyn EventSink,
    ) -> String {
        sink.emit(AgentEvent::ToolStarted {
            id: call.id.clone(),
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        });

        let result = self.run_tool(policy, call).await;
        let is_error = result.starts_with(ERROR_PREFIX);

        sink.emit(AgentEvent::ToolFinished {
            id: call.id.clone(),
            name: call.function.name.clone(),
            result: result.clone(),
            is_error,
        });
        result
    }

    /// Parses arguments, looks up the tool, gates `Risky` tools through
    /// `policy`, and executes. Every failure path returns an `ERROR:`-prefixed
    /// string (never panics, never aborts the turn).
    async fn run_tool(&self, policy: &mut dyn PermissionPolicy, call: &ToolCall) -> String {
        let args = match call.function.parse_arguments() {
            Ok(v) => v,
            Err(e) => return format!("{ERROR_PREFIX} bad arguments JSON: {e}"),
        };

        let Some(tool) = self.registry.get(&call.function.name) else {
            return format!("{ERROR_PREFIX} unknown tool '{}'", call.function.name);
        };

        if tool.risk_level() == RiskLevel::Risky {
            let decision = policy.decide(tool.name(), &call.function.arguments, RiskLevel::Risky);
            if !decision.is_allow() {
                return format!("{ERROR_PREFIX} permission denied for '{}'", tool.name());
            }
        }

        match tool.execute(args).await {
            Ok(out) => out.content,
            Err(e) => format!("{ERROR_PREFIX} {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{ToolError, ToolOutput};
    use crate::{MockProvider, Tool};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
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
            serde_json::json!({ "type": "object" })
        }
        fn risk_level(&self) -> RiskLevel {
            RiskLevel::Safe
        }
        async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(args.to_string()))
        }
    }

    /// A `Risky` tool that flips a shared flag when it actually executes — so
    /// a test can assert it was (or was not) reached past the permission gate.
    struct FlagRisky {
        executed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Tool for FlagRisky {
        fn name(&self) -> &str {
            "danger"
        }
        fn description(&self) -> &str {
            "a risky tool"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn risk_level(&self) -> RiskLevel {
            RiskLevel::Risky
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<ToolOutput, ToolError> {
            self.executed.store(true, Ordering::SeqCst);
            Ok(ToolOutput::new("danger ran"))
        }
    }

    fn collecting_sink(buf: &mut Vec<AgentEvent>) -> impl FnMut(AgentEvent) + '_ {
        move |ev| buf.push(ev)
    }

    #[tokio::test]
    async fn text_only_turn_returns_and_emits_content() {
        let provider = MockProvider::new();
        provider.push_text("hello world");
        let registry = ToolRegistry::new();
        let defs = registry.definitions();
        let agent = Agent::new(&provider, &registry, &defs);

        let mut session = Session::new("sys");
        session.push_user("hi");
        let mut policy = AllowlistPolicy::deny_all();
        let mut events = Vec::new();
        let mut sink = collecting_sink(&mut events);

        let outcome = agent
            .run_turn(&mut session, "m", &mut policy, &mut sink)
            .await
            .unwrap();
        drop(sink); // release the &mut events borrow before reading events

        assert_eq!(outcome.content, "hello world");
        assert!(!outcome.hit_iteration_limit);
        assert!(events.contains(&AgentEvent::Content("hello world".into())));
        // user + assistant on top of the system prompt.
        assert_eq!(session.messages().len(), 3);
    }

    #[tokio::test]
    async fn tool_call_then_final_answer_feeds_result_back() {
        let provider = MockProvider::new();
        provider.push_tool_call("c1", "echo", r#"{"x":1}"#);
        provider.push_text("done");
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let defs = registry.definitions();
        let agent = Agent::new(&provider, &registry, &defs);

        let mut session = Session::new("sys");
        session.push_user("use echo");
        // echo is Safe, so deny_all still lets it run.
        let mut policy = AllowlistPolicy::deny_all();
        let mut events = Vec::new();
        let mut sink = collecting_sink(&mut events);

        let outcome = agent
            .run_turn(&mut session, "m", &mut policy, &mut sink)
            .await
            .unwrap();
        drop(sink); // release the &mut events borrow before reading events

        assert_eq!(outcome.content, "done");
        // ToolStarted + ToolFinished(success) emitted for echo.
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolStarted { name, .. } if name == "echo")));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolFinished { name, is_error, .. } if name == "echo" && !is_error
        )));
        // A tool-result message made it into the history.
        assert!(session
            .messages()
            .iter()
            .any(|m| m.role == "tool" && m.content.contains("\"x\":1")));
    }

    #[tokio::test]
    async fn risky_tool_denied_by_default() {
        let executed = Arc::new(AtomicBool::new(false));
        let provider = MockProvider::new();
        provider.push_tool_call("c1", "danger", "{}");
        provider.push_text("ok, skipped");
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FlagRisky {
            executed: executed.clone(),
        }));
        let defs = registry.definitions();
        let agent = Agent::new(&provider, &registry, &defs);

        let mut session = Session::new("sys");
        session.push_user("do danger");
        let mut policy = AllowlistPolicy::deny_all();
        let mut sink = |_ev: AgentEvent| {};

        let outcome = agent
            .run_turn(&mut session, "m", &mut policy, &mut sink)
            .await
            .unwrap();

        assert!(!executed.load(Ordering::SeqCst), "denied tool must not run");
        assert_eq!(outcome.content, "ok, skipped");
        assert!(session
            .messages()
            .iter()
            .any(|m| m.role == "tool" && m.content.contains("permission denied")));
    }

    #[tokio::test]
    async fn risky_tool_allowed_via_allowlist_runs() {
        let executed = Arc::new(AtomicBool::new(false));
        let provider = MockProvider::new();
        provider.push_tool_call("c1", "danger", "{}");
        provider.push_text("did it");
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FlagRisky {
            executed: executed.clone(),
        }));
        let defs = registry.definitions();
        let agent = Agent::new(&provider, &registry, &defs);

        let mut session = Session::new("sys");
        session.push_user("do danger");
        let mut policy = AllowlistPolicy::from_allowed(["danger"]);
        let mut sink = |_ev: AgentEvent| {};

        agent
            .run_turn(&mut session, "m", &mut policy, &mut sink)
            .await
            .unwrap();

        assert!(executed.load(Ordering::SeqCst), "allowed tool must run");
        assert!(session
            .messages()
            .iter()
            .any(|m| m.role == "tool" && m.content == "danger ran"));
    }

    #[tokio::test]
    async fn iteration_limit_stops_and_flags() {
        let provider = MockProvider::new();
        // Always ask for a tool, never give a final text answer.
        for _ in 0..(MAX_TOOL_ITERATIONS + 2) {
            provider.push_tool_call("c", "echo", "{}");
        }
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let defs = registry.definitions();
        let agent = Agent::new(&provider, &registry, &defs);

        let mut session = Session::new("sys");
        session.push_user("loop forever");
        let mut policy = AllowlistPolicy::deny_all();
        let mut events = Vec::new();
        let mut sink = collecting_sink(&mut events);

        let outcome = agent
            .run_turn(&mut session, "m", &mut policy, &mut sink)
            .await
            .unwrap();
        drop(sink); // release the &mut events borrow before reading events

        assert!(outcome.hit_iteration_limit);
        assert!(events.contains(&AgentEvent::IterationLimit));
    }

    #[test]
    fn allowlist_policy_decisions() {
        let mut deny = AllowlistPolicy::deny_all();
        assert_eq!(
            deny.decide("bash", "{}", RiskLevel::Risky),
            Decision::DenyOnce
        );

        let mut allow = AllowlistPolicy::from_allowed(["bash", "edit"]);
        assert_eq!(
            allow.decide("bash", "{}", RiskLevel::Risky),
            Decision::AllowOnce
        );
        assert_eq!(
            allow.decide("write_file", "{}", RiskLevel::Risky),
            Decision::DenyOnce
        );

        let mut all = AllowlistPolicy::allow_all();
        assert_eq!(
            all.decide("anything", "{}", RiskLevel::Risky),
            Decision::AllowOnce
        );
    }
}
