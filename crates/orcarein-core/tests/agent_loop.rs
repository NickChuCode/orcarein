//! End-to-end example of driving the agent engine from **outside** the crate —
//! the exact shape an embedder (or Ch24's issue bot) will use. No network: the
//! `MockProvider` scripts the model's turns. This is the library-first proof —
//! everything used here is public API.
//!
//! Run with: `cargo test -p orcarein-core --test agent_loop`

use async_trait::async_trait;
use orcarein_core::{
    Agent, AgentEvent, AllowlistPolicy, MockProvider, RiskLevel, Session, Tool, ToolError,
    ToolOutput, ToolRegistry,
};

/// A tiny `Risky` tool an embedder might register. Returns a fixed result so
/// the example stays deterministic and offline.
struct StubWrite;

#[async_trait]
impl Tool for StubWrite {
    fn name(&self) -> &str {
        "stub_write"
    }
    fn description(&self) -> &str {
        "pretend to write a file"
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Risky
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("wrote 3 lines"))
    }
}

/// Builds the registry + scripted provider shared by the two examples: the
/// model asks for `stub_write`, then (after seeing the result) replies.
fn scripted() -> (MockProvider, ToolRegistry) {
    let provider = MockProvider::new();
    provider.push_tool_call("c1", "stub_write", r#"{"path":"out.txt"}"#);
    provider.push_text("Done — wrote the file.");

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(StubWrite));
    (provider, registry)
}

#[tokio::test]
async fn embedder_runs_a_tool_using_turn_when_allowed() {
    let (provider, registry) = scripted();
    let defs = registry.definitions();
    let agent = Agent::new(&provider, &registry, &defs);

    let mut session = Session::new("you are a helpful assistant");
    session.push_user("write out.txt");

    // Grant the one Risky tool we expect; everything else stays denied.
    let mut policy = AllowlistPolicy::from_allowed(["stub_write"]);
    let mut events: Vec<AgentEvent> = Vec::new();

    let outcome = agent
        .run_turn(
            &mut session,
            "mock-model",
            &mut policy,
            &mut |e: AgentEvent| events.push(e),
        )
        .await
        .expect("turn should not error");

    assert_eq!(outcome.content, "Done — wrote the file.");
    assert!(!outcome.hit_iteration_limit);
    // The tool actually ran (no error) ...
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished { name, is_error, .. } if name == "stub_write" && !is_error
    )));
    // ... and its output was fed back into the conversation.
    assert!(session
        .messages()
        .iter()
        .any(|m| m.role == "tool" && m.content == "wrote 3 lines"));
}

#[tokio::test]
async fn embedder_denies_risky_tool_by_default() {
    let (provider, registry) = scripted();
    let defs = registry.definitions();
    let agent = Agent::new(&provider, &registry, &defs);

    let mut session = Session::new("you are a helpful assistant");
    session.push_user("write out.txt");

    // Deny-by-default: the headless safe posture.
    let mut policy = AllowlistPolicy::deny_all();
    let mut events: Vec<AgentEvent> = Vec::new();

    let outcome = agent
        .run_turn(
            &mut session,
            "mock-model",
            &mut policy,
            &mut |e: AgentEvent| events.push(e),
        )
        .await
        .expect("a denied tool is not an error — the model still answers");

    // The model still produced its final reply; the denial was fed back to it.
    assert_eq!(outcome.content, "Done — wrote the file.");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished { name, is_error, .. } if name == "stub_write" && *is_error
    )));
    assert!(session
        .messages()
        .iter()
        .any(|m| m.role == "tool" && m.content.contains("permission denied")));
}
