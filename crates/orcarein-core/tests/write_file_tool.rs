//! Integration tests for `WriteFileTool`.

use orcarein_core::{Tool, ToolError, WriteFileTool};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn writes_a_new_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("out.txt");
    let tool = WriteFileTool;

    let out = tool
        .execute(json!({ "path": path.to_string_lossy(), "content": "hello" }))
        .await
        .expect("write should succeed");

    assert!(out.content.contains("Wrote 5 bytes"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
}

#[tokio::test]
async fn overwrites_an_existing_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("out.txt");
    std::fs::write(&path, "first").unwrap();

    let tool = WriteFileTool;
    tool.execute(json!({ "path": path.to_string_lossy(), "content": "second" }))
        .await
        .expect("overwrite should succeed");

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
}

#[tokio::test]
async fn missing_parent_dir_is_an_io_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nope").join("nested.txt");

    let tool = WriteFileTool;
    let err = tool
        .execute(json!({ "path": path.to_string_lossy(), "content": "x" }))
        .await
        .expect_err("missing parent dir should error");

    assert!(matches!(err, ToolError::Io(_)), "expected Io, got {err:?}");
}
