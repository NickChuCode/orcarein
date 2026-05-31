//! Integration test for `ReadFileTool` — OrcaRein's first cross-crate test.
//!
//! Lives in `tests/` (separate compilation unit) so it can only touch the
//! crate's public API. Anything missing here is a public-API regression.

use orcarein_core::{ReadFileTool, RiskLevel, Tool, ToolError};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn reads_an_existing_file() {
    let dir = tempdir().expect("create tempdir");
    let path = dir.path().join("hello.txt");
    std::fs::write(&path, "Hello, tool!").expect("seed temp file");

    let tool = ReadFileTool;
    let out = tool
        .execute(json!({ "path": path.to_string_lossy() }))
        .await
        .expect("read_file should succeed");

    assert_eq!(out.content, "Hello, tool!");
}

#[tokio::test]
async fn missing_file_is_an_io_error() {
    let dir = tempdir().expect("create tempdir");
    let path = dir.path().join("nope-does-not-exist.txt");

    let tool = ReadFileTool;
    let err = tool
        .execute(json!({ "path": path.to_string_lossy() }))
        .await
        .expect_err("nonexistent path should error");

    assert!(
        matches!(err, ToolError::Io(_)),
        "expected ToolError::Io, got {err:?}",
    );
}

#[tokio::test]
async fn rejects_args_without_path() {
    let tool = ReadFileTool;
    let err = tool
        .execute(json!({}))
        .await
        .expect_err("missing `path` should error");

    assert!(
        matches!(err, ToolError::InvalidArguments(_)),
        "expected ToolError::InvalidArguments, got {err:?}",
    );
}

#[test]
fn metadata_is_correct() {
    let t = ReadFileTool;
    assert_eq!(t.name(), "read_file");
    assert_eq!(t.risk_level(), RiskLevel::Safe);

    let schema = t.schema();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["path"]["type"], "string");
    assert!(schema["required"]
        .as_array()
        .expect("required is an array")
        .iter()
        .any(|v| v == "path"));
}
