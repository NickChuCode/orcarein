//! Integration tests for `EditTool`.

use deeprig_core::{EditTool, Tool, ToolError};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn replaces_unique_substring() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "foo bar baz").unwrap();

    let tool = EditTool;
    let out = tool
        .execute(json!({
            "path": path.to_string_lossy(),
            "old_str": "bar",
            "new_str": "qux",
        }))
        .await
        .expect("unique replacement should succeed");

    assert!(out.content.contains("1 replacement"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo qux baz");
}

#[tokio::test]
async fn missing_substring_errors() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "no needle here").unwrap();

    let tool = EditTool;
    let err = tool
        .execute(json!({
            "path": path.to_string_lossy(),
            "old_str": "needle999",
            "new_str": "x",
        }))
        .await
        .expect_err("missing substring should error");

    match err {
        ToolError::Other(msg) => assert!(msg.contains("not found"), "got: {msg}"),
        other => panic!("expected Other, got {other:?}"),
    }
    // File untouched.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "no needle here");
}

#[tokio::test]
async fn multiple_occurrences_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "aaa").unwrap();

    let tool = EditTool;
    let err = tool
        .execute(json!({
            "path": path.to_string_lossy(),
            "old_str": "a",
            "new_str": "b",
        }))
        .await
        .expect_err("non-unique should error");

    match err {
        ToolError::Other(msg) => {
            assert!(msg.contains("3 times"), "got: {msg}");
            assert!(msg.contains("must be unique"), "got: {msg}");
        }
        other => panic!("expected Other, got {other:?}"),
    }
    // File untouched.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "aaa");
}

#[tokio::test]
async fn identical_strings_error_without_reading() {
    let tool = EditTool;
    // Path does not exist; the identical-strings fast-fail must trigger
    // before any IO so this is `Other`, not `Io`.
    let err = tool
        .execute(json!({
            "path": "/this/path/does/not/exist.txt",
            "old_str": "x",
            "new_str": "x",
        }))
        .await
        .expect_err("identical strings should error");

    match err {
        ToolError::Other(msg) => assert!(msg.contains("identical"), "got: {msg}"),
        other => panic!("expected Other (no IO performed), got {other:?}"),
    }
}
