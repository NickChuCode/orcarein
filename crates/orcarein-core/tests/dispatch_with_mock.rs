//! End-to-end dispatcher seam test using `MockProvider`.
//!
//! Until Ch13 the dispatch loop was only exercised through the live
//! REPL — there was no way to assert "model asks for read_file, we
//! execute it, model gets the content, model replies". This test
//! closes that gap by scripting the mock provider to return a
//! `read_file` tool call first and a plain text reply second, then
//! drives a minimal dispatch loop against the real `ReadFileTool` over
//! a `tempfile` fixture.
//!
//! The same shape will let Ch24's issue-bot scenarios be tested
//! offline.

use futures_util::StreamExt;
use orcarein_core::{
    ChatOptions, FunctionCall, MockProvider, Provider, ReadFileTool, StreamEvent, Tool, ToolCall,
    ToolError, ToolOutput,
};
use serde_json::json;
use tempfile::tempdir;

/// Drives one chat_stream call to completion, collecting any
/// tool_calls the mock emits.
async fn collect_tool_calls(provider: &dyn Provider) -> Vec<ToolCall> {
    let opts = ChatOptions::new("any");
    let mut s = provider.chat_stream(&[], &[], &opts).await.unwrap();
    let mut calls = Vec::new();
    while let Some(ev) = s.next().await {
        if let StreamEvent::ToolCalls(c) = ev.unwrap() {
            calls = c;
        }
    }
    calls
}

#[tokio::test]
async fn mock_round_trip_with_read_file() {
    // Fixture: a temp file the mock will tell the dispatcher to read.
    let dir = tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "hello from disk").unwrap();
    let path_str = path.to_string_lossy().into_owned();

    let mock = MockProvider::new();

    // Turn 1: the model wants to call read_file.
    mock.push_tool_call(
        "call_1",
        "read_file",
        &json!({ "path": path_str }).to_string(),
    );
    // Turn 2: now that we've fed back the content, the model replies.
    mock.push_text("ok, I saw the file");

    // Turn 1.
    let calls = collect_tool_calls(&mock).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "read_file");

    // Dispatch the tool the same way main.rs's `dispatch` would.
    let tool = ReadFileTool;
    let args = calls[0].function.parse_arguments().unwrap();
    let out: ToolOutput = tool.execute(args).await.unwrap();
    assert_eq!(out.content, "hello from disk");

    // Turn 2: model takes the result and replies.
    let opts = ChatOptions::new("any");
    let mut s = mock.chat_stream(&[], &[], &opts).await.unwrap();
    let mut got = String::new();
    while let Some(ev) = s.next().await {
        if let StreamEvent::Content(t) = ev.unwrap() {
            got.push_str(&t);
        }
    }
    assert_eq!(got, "ok, I saw the file");
    assert_eq!(mock.pending(), 0);
}

#[tokio::test]
async fn tool_error_surfaces_through_provider_seam() {
    // The mock claims a file path that does not exist; ReadFileTool
    // returns Err. We assert the dispatcher path can observe the error
    // type so it can build the `ERROR: ...` payload (main.rs::dispatch
    // does this; here we verify the error variant directly).
    let mock = MockProvider::new();
    mock.push_tool_call(
        "call_1",
        "read_file",
        r#"{"path":"/this/path/does/not/exist.txt"}"#,
    );

    let calls = collect_tool_calls(&mock).await;
    assert_eq!(calls.len(), 1);

    let tool = ReadFileTool;
    let args = calls[0].function.parse_arguments().unwrap();
    let err = tool.execute(args).await.unwrap_err();
    assert!(
        matches!(err, ToolError::Io(_)),
        "expected ToolError::Io, got {err:?}"
    );
}

#[tokio::test]
async fn malformed_tool_arguments_caught_by_function_call_parse() {
    // The mock returns a tool call whose `arguments` string is not
    // valid JSON. The dispatcher detects this via
    // `FunctionCall::parse_arguments`, before touching the tool.
    let mock = MockProvider::new();
    mock.push_response(vec![StreamEvent::ToolCalls(vec![ToolCall {
        id: "call_1".into(),
        kind: "function".into(),
        function: FunctionCall {
            name: "read_file".into(),
            arguments: "not json at all".into(),
        },
    }])]);

    let calls = collect_tool_calls(&mock).await;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].function.parse_arguments().is_err());
}
