use orcarein_core::provider::testing::MockProvider;
use orcarein_core::{StreamEvent, TokenUsage};
use orcarein_eval::runner::{run_case, standard_registry, RunConfig};
use orcarein_eval::task::{toy_suite, Verdict};
use orcarein_core::CacheMode;

#[tokio::test]
async fn run_case_executes_a_write_and_grades_pass() {
    // Script the mock: first a tool call that writes hello.txt, then a final
    // text reply carrying usage. The mock ignores prompt content, so this is a
    // plumbing test (cache delta is a real-API phenomenon, not testable here).
    let provider = MockProvider::new();
    provider.push_tool_call(
        "call_1",
        "write_file",
        r#"{"path":"hello.txt","content":"hello world\n"}"#,
    );
    provider.push_response(vec![
        StreamEvent::Content("done".into()),
        StreamEvent::Usage(TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 20,
            total_tokens: 1020,
            prompt_cache_hit_tokens: 800,
            prompt_cache_miss_tokens: 200,
        }),
    ]);

    let registry = standard_registry();
    let defs = registry.definitions();
    let case = toy_suite().into_iter().find(|c| c.id == "create-hello").unwrap();
    let cfg = RunConfig {
        cache_mode: CacheMode::Economy,
        model: "mock-model".into(),
        allow_tools: vec!["write_file".into()],
        max_iterations: 4,
    };

    let rec = run_case(&case, &cfg, &provider, &registry, &defs).await.unwrap();

    assert_eq!(rec.verdict, Verdict::Pass);
    assert_eq!(rec.usage.prompt_tokens, 1000);
    assert_eq!(rec.usage.prompt_cache_hit_tokens, 800);
    assert!(rec.steps >= 1, "at least one tool call");
    assert!(!rec.trace.is_empty());
}
