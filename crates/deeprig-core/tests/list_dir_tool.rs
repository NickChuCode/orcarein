//! Integration tests for `ListDirTool`.

use deeprig_core::{ListDirTool, Tool, ToolError};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn lists_files_and_subdirs_sorted() {
    let dir = tempdir().unwrap();
    // Create in non-alphabetical order to verify sorting.
    std::fs::write(dir.path().join("b.txt"), "").unwrap();
    std::fs::write(dir.path().join("a.txt"), "").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();

    let tool = ListDirTool;
    let out = tool
        .execute(json!({ "path": dir.path().to_string_lossy() }))
        .await
        .expect("list should succeed");

    assert_eq!(out.content, "f a.txt\nf b.txt\nd sub/\n");
}

#[tokio::test]
async fn empty_dir_returns_empty_output() {
    let dir = tempdir().unwrap();
    let tool = ListDirTool;
    let out = tool
        .execute(json!({ "path": dir.path().to_string_lossy() }))
        .await
        .expect("empty dir should succeed");
    assert!(out.content.is_empty());
}

#[tokio::test]
async fn nonexistent_path_is_an_io_error() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("not-there");
    let tool = ListDirTool;
    let err = tool
        .execute(json!({ "path": missing.to_string_lossy() }))
        .await
        .expect_err("nonexistent path should error");
    assert!(matches!(err, ToolError::Io(_)), "expected Io, got {err:?}");
}
