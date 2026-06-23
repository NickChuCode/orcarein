//! Runs one (case × config) cell: a fresh temp workspace, a fresh Session, the
//! core headless Agent, a sink that captures the trace + first-token time.

use std::time::Instant;

use anyhow::Result;
use orcarein_core::{
    Agent, AgentEvent, AllowlistPolicy, BashTool, CacheMode, EditTool, EventSink, ListDirTool,
    Provider, ReadFileTool, Session, TokenUsage, ToolDefinition, ToolRegistry, WriteFileTool,
};

use crate::task::{EvalCase, Verdict};

/// System prompt for eval runs — minimal and stable (cache-friendly).
const EVAL_SYSTEM_PROMPT: &str =
    "You are a coding agent operating in a scratch workspace. Use the provided \
     tools to complete the task. Keep actions minimal.";

/// Per-cell knobs. Everything here is fixed for reproducibility.
pub struct RunConfig {
    pub cache_mode: CacheMode,
    pub model: String,
    /// Risky tools the run is allowed to use (fed to `AllowlistPolicy`).
    pub allow_tools: Vec<String>,
    pub max_iterations: usize,
}

/// Raw outcome of one run, before aggregation into a metrics row.
pub struct RunRecord {
    pub usage: TokenUsage,
    pub verdict: Verdict,
    pub steps: usize,
    pub wall_ms: u64,
    pub ttft_ms: Option<u64>,
    pub trace: Vec<AgentEvent>,
}

/// Builds the standard 5-tool registry (read/write/list/bash/edit), same set as
/// the bin. All tools are registered; `AllowlistPolicy` does the risk gating.
pub fn standard_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Box::new(ReadFileTool));
    r.register(Box::new(WriteFileTool));
    r.register(Box::new(ListDirTool));
    r.register(Box::new(BashTool));
    r.register(Box::new(EditTool));
    r
}

/// Collects every event and records when the first content/reasoning arrived.
struct CollectingSink {
    events: Vec<AgentEvent>,
    tool_starts: usize,
    started: Instant,
    ttft_ms: Option<u64>,
}

impl CollectingSink {
    fn new(started: Instant) -> Self {
        CollectingSink {
            events: Vec::new(),
            tool_starts: 0,
            started,
            ttft_ms: None,
        }
    }
}

impl EventSink for CollectingSink {
    fn emit(&mut self, event: AgentEvent) {
        match &event {
            AgentEvent::Content(_) | AgentEvent::Reasoning(_) if self.ttft_ms.is_none() => {
                self.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
            }
            AgentEvent::Content(_) | AgentEvent::Reasoning(_) => {}
            AgentEvent::ToolStarted { .. } => self.tool_starts += 1,
            _ => {}
        }
        self.events.push(event);
    }
}

/// Runs one case under one config against `provider`, returning the raw record.
///
/// NOTE: changes the process CWD into a temp dir for the duration of the run
/// (the file tools are CWD-relative) and restores it after. This makes the
/// function NON-reentrant — callers must run cells sequentially.
pub async fn run_case(
    case: &EvalCase,
    cfg: &RunConfig,
    provider: &dyn Provider,
    registry: &ToolRegistry,
    defs: &[ToolDefinition],
) -> Result<RunRecord> {
    let dir = tempfile::tempdir()?;
    (case.setup)(dir.path())?;

    // A fresh session per case => session.usage() is exactly this task's total.
    let mut session = Session::new(EVAL_SYSTEM_PROMPT);
    session.push_user(&case.prompt);

    let mut policy = AllowlistPolicy::from_allowed(cfg.allow_tools.clone());
    let started = Instant::now();
    let mut sink = CollectingSink::new(started);

    let agent = Agent::new(provider, registry, defs)
        .with_cache_mode(cfg.cache_mode)
        .with_max_iterations(cfg.max_iterations);

    // The file tools resolve paths relative to the process CWD, so run inside
    // `dir`. Process-global => callers must run cells sequentially.
    let prev_cwd = std::env::current_dir()?;
    std::env::set_current_dir(dir.path())?;
    let result = agent
        .run_turn(&mut session, &cfg.model, &mut policy, &mut sink)
        .await;
    std::env::set_current_dir(prev_cwd)?;
    result?;

    let verdict = (case.grader)(dir.path());

    Ok(RunRecord {
        usage: session.usage(),
        verdict,
        steps: sink.tool_starts,
        wall_ms: started.elapsed().as_millis() as u64,
        ttft_ms: sink.ttft_ms,
        trace: sink.events,
    })
}
