//! Per-task metrics: aggregate a run's raw outcome into one CSV-ready row.
//!
//! Cost & cache fields come from `orcarein_core::cost::estimate` — the single
//! source of truth — so they never drift from what the REPL's `/usage` shows.

use orcarein_core::{cost, TokenUsage};

use crate::task::Verdict;

/// One row of results: one (task × config × repeat) cell.
#[derive(Debug, Clone)]
pub struct TaskMetrics {
    pub task_id: String,
    pub config: String,
    pub repeat: usize,
    pub verdict: Verdict,
    pub steps: usize,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub cache_hit_rate: f64,
    pub completion_tokens: u64,
    pub spent_usd: f64,
    pub saved_usd: f64,
    pub input_saved_pct: f64,
    pub wall_ms: u64,
    pub ttft_ms: Option<u64>,
}

/// Builds a [`TaskMetrics`] from a run's raw outcome plus its labels.
///
/// `cache_hit_rate`/`spent_usd`/`saved_usd`/`input_saved_pct` are taken from
/// `cost::estimate`; when the model's prices are unknown they default to 0.
#[allow(clippy::too_many_arguments)]
pub fn aggregate(
    usage: &TokenUsage,
    verdict: Verdict,
    steps: usize,
    wall_ms: u64,
    ttft_ms: Option<u64>,
    task_id: &str,
    config: &str,
    repeat: usize,
    model: &str,
) -> TaskMetrics {
    let est = cost::estimate(usage, model);
    TaskMetrics {
        task_id: task_id.to_string(),
        config: config.to_string(),
        repeat,
        verdict,
        steps,
        prompt_tokens: usage.prompt_tokens,
        cached_tokens: usage.prompt_cache_hit_tokens,
        cache_hit_rate: est.map(|e| e.hit_rate).unwrap_or(0.0),
        completion_tokens: usage.completion_tokens,
        spent_usd: est.map(|e| e.spent_usd).unwrap_or(0.0),
        saved_usd: est.map(|e| e.saved_usd).unwrap_or(0.0),
        input_saved_pct: est.map(|e| e.input_saved_pct).unwrap_or(0.0),
        wall_ms,
        ttft_ms,
    }
}
