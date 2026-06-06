//! Integration tests for `EditTool`.

use orcarein_core::{EditTool, Tool, ToolError};
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

// ---- v0.2 D: reliability hardening ----

#[tokio::test]
async fn crlf_file_matches_lf_old_str() {
    // The file uses Windows CRLF; the model sends a multi-line old_str with
    // plain LF. Line-ending tolerance must still match and edit it.
    let dir = tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "alpha\r\nbeta\r\ngamma").unwrap();

    let tool = EditTool;
    let out = tool
        .execute(json!({
            "path": path.to_string_lossy(),
            "old_str": "alpha\nbeta",   // LF, spanning a line boundary
            "new_str": "ALPHA\nBETA",
        }))
        .await
        .expect("CRLF/LF mismatch should still match");

    assert!(out.content.contains("1 replacement"));
    // The file keeps its CRLF endings.
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "ALPHA\r\nBETA\r\ngamma"
    );
}

#[tokio::test]
async fn whitespace_only_mismatch_is_diagnosed_not_applied() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("code.py");
    std::fs::write(&path, "if x:\n    do_thing()\n").unwrap();

    let tool = EditTool;
    let err = tool
        .execute(json!({
            "path": path.to_string_lossy(),
            "old_str": "if x:\n  do_thing()",   // 2-space indent vs file's 4
            "new_str": "if x:\n  do_other()",
        }))
        .await
        .expect_err("indentation mismatch should error, not silently edit");

    match err {
        ToolError::Other(msg) => {
            assert!(msg.contains("whitespace/indentation"), "got: {msg}");
        }
        other => panic!("expected Other, got {other:?}"),
    }
    // File untouched — no silent wrong edit.
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "if x:\n    do_thing()\n"
    );
}

#[tokio::test]
async fn ambiguous_match_reports_line_numbers() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("doc.txt");
    std::fs::write(&path, "x\nFOO\ny\nFOO\nz").unwrap();

    let tool = EditTool;
    let err = tool
        .execute(json!({
            "path": path.to_string_lossy(),
            "old_str": "FOO",
            "new_str": "BAR",
        }))
        .await
        .expect_err("non-unique should error");

    match err {
        ToolError::Other(msg) => {
            assert!(msg.contains("2 times"), "got: {msg}");
            assert!(msg.contains("line(s) 2, 4"), "got: {msg}");
            assert!(msg.contains("must be unique"), "got: {msg}");
        }
        other => panic!("expected Other, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_file_has_clear_message() {
    let tool = EditTool;
    let err = tool
        .execute(json!({
            "path": "/no/such/file/here.txt",
            "old_str": "a",
            "new_str": "b",
        }))
        .await
        .expect_err("missing file should error");

    match err {
        ToolError::Other(msg) => assert!(msg.contains("file not found"), "got: {msg}"),
        other => panic!("expected a clear Other message, got {other:?}"),
    }
}

#[tokio::test]
async fn oversized_file_is_refused() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("big.txt");
    // Just over the 5 MiB limit.
    std::fs::write(&path, "x".repeat(5 * 1024 * 1024 + 16)).unwrap();

    let tool = EditTool;
    let err = tool
        .execute(json!({
            "path": path.to_string_lossy(),
            "old_str": "zzz",
            "new_str": "qqq",
        }))
        .await
        .expect_err("oversized file should be refused");

    match err {
        ToolError::Other(msg) => assert!(msg.contains("too large"), "got: {msg}"),
        other => panic!("expected Other, got {other:?}"),
    }
}
