use orcarein_core::TokenUsage;
use orcarein_eval::metrics::{aggregate, TaskMetrics};
use orcarein_eval::task::Verdict;

fn usage(prompt: u64, hit: u64, miss: u64, completion: u64) -> TokenUsage {
    TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        prompt_cache_hit_tokens: hit,
        prompt_cache_miss_tokens: miss,
    }
}

#[test]
fn aggregate_carries_tokens_and_computes_cache_hit_rate() {
    let u = usage(1000, 800, 200, 100);
    let m: TaskMetrics = aggregate(
        &u,
        Verdict::Pass,
        3,
        1200,
        Some(80),
        "create-hello",
        "economy",
        0,
        "deepseek-v4-flash",
    );

    assert_eq!(m.task_id, "create-hello");
    assert_eq!(m.config, "economy");
    assert_eq!(m.repeat, 0);
    assert_eq!(m.verdict, Verdict::Pass);
    assert_eq!(m.steps, 3);
    assert_eq!(m.prompt_tokens, 1000);
    assert_eq!(m.cached_tokens, 800);
    assert_eq!(m.completion_tokens, 100);
    assert_eq!(m.wall_ms, 1200);
    assert_eq!(m.ttft_ms, Some(80));
    // hit_rate sourced from cost::estimate (800 / 1000).
    assert!((m.cache_hit_rate - 0.8).abs() < 1e-9);
    // Known DeepSeek model => cost fields populated and positive.
    assert!(m.spent_usd > 0.0);
    assert!(m.saved_usd > 0.0);
    assert!(m.input_saved_pct > 0.0);
}

#[test]
fn aggregate_unknown_model_zeroes_cost_fields() {
    let u = usage(1000, 800, 200, 100);
    let m = aggregate(
        &u,
        Verdict::Fail,
        1,
        50,
        None,
        "x",
        "benchmark",
        2,
        "gpt-4o",
    );
    // Unknown model => cost::estimate returns None => cost/cache fields are 0.
    assert_eq!(m.spent_usd, 0.0);
    assert_eq!(m.saved_usd, 0.0);
    assert_eq!(m.input_saved_pct, 0.0);
    assert_eq!(m.cache_hit_rate, 0.0);
    // Token counts still carried through from usage.
    assert_eq!(m.cached_tokens, 800);
    assert_eq!(m.ttft_ms, None);
}
