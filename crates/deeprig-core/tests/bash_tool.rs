//! Integration tests for `BashTool`.
//!
//! These exercise the cross-platform shim — `cmd /C` on Windows,
//! `bash -c` elsewhere. Use commands that behave identically under both.

use deeprig_core::{BashTool, Tool};
use serde_json::json;

#[tokio::test]
async fn runs_echo_and_captures_stdout() {
    let tool = BashTool;
    let out = tool
        .execute(json!({ "command": "echo hello" }))
        .await
        .expect("echo should succeed");
    assert!(out.content.contains("exit_code: 0"), "got: {}", out.content);
    assert!(out.content.contains("hello"), "got: {}", out.content);
}

#[tokio::test]
async fn nonzero_exit_is_ok_not_err() {
    let tool = BashTool;
    let out = tool
        .execute(json!({ "command": "exit 3" }))
        .await
        .expect("non-zero exit should still be Ok");
    assert!(out.content.contains("exit_code: 3"), "got: {}", out.content);
}
