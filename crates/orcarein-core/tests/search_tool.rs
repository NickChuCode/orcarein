//! Integration tests for `SearchTool` (v02-19).
//!
//! Uses a tempdir tree so assertions don't depend on the host filesystem.
//! Output paths are relative to the search root and use `/` separators on
//! every platform, so these assertions hold on Windows and Linux alike.

use orcarein_core::{SearchTool, Tool, ToolError};
use serde_json::json;
use tempfile::tempdir;

/// Writes `contents` to `dir/rel`, creating parent directories as needed.
fn write(dir: &std::path::Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn content_match_reports_path_line_text_sorted() {
    let dir = tempdir().unwrap();
    write(dir.path(), "src/lib.rs", "fn hello() {}\nfn world() {}\n");
    write(dir.path(), "notes.txt", "say hello there\n");

    let out = SearchTool
        .execute(json!({ "pattern": "hello", "path": dir.path().to_string_lossy() }))
        .await
        .expect("search should succeed");

    // Sorted by (path, line); paths relative with `/`; line text trimmed.
    assert_eq!(
        out.content,
        "notes.txt:1:say hello there\nsrc/lib.rs:1:fn hello() {}\n"
    );
}

#[tokio::test]
async fn no_match_returns_no_matches() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.txt", "nothing here\n");

    let out = SearchTool
        .execute(json!({ "pattern": "zzz", "path": dir.path().to_string_lossy() }))
        .await
        .expect("no-match is not an error");

    assert_eq!(out.content, "no matches");
}

#[tokio::test]
async fn glob_filters_by_filename() {
    let dir = tempdir().unwrap();
    write(dir.path(), "keep.rs", "let hello = 1;\n");
    write(dir.path(), "skip.txt", "hello world\n");

    let out = SearchTool
        .execute(json!({
            "pattern": "hello",
            "path": dir.path().to_string_lossy(),
            "glob": "*.rs",
        }))
        .await
        .expect("search should succeed");

    assert_eq!(out.content, "keep.rs:1:let hello = 1;\n");
}

#[tokio::test]
async fn case_insensitive_matches_mixed_case() {
    let dir = tempdir().unwrap();
    write(dir.path(), "a.txt", "Hello HELLO hELLo\n");

    let out = SearchTool
        .execute(json!({
            "pattern": "hello",
            "path": dir.path().to_string_lossy(),
            "case_insensitive": true,
        }))
        .await
        .expect("search should succeed");

    assert_eq!(out.content, "a.txt:1:Hello HELLO hELLo\n");
}

#[tokio::test]
async fn output_mode_files_lists_unique_sorted_paths() {
    let dir = tempdir().unwrap();
    write(dir.path(), "b.txt", "hit\nhit again\n"); // two matches, one file
    write(dir.path(), "a.txt", "hit\n");

    let out = SearchTool
        .execute(json!({
            "pattern": "hit",
            "path": dir.path().to_string_lossy(),
            "output_mode": "files",
        }))
        .await
        .expect("search should succeed");

    // Each matching file once, sorted — no line numbers or text.
    assert_eq!(out.content, "a.txt\nb.txt\n");
}

#[tokio::test]
async fn output_mode_count_reports_matches_per_file() {
    let dir = tempdir().unwrap();
    write(dir.path(), "b.txt", "hit\nhit again\nnope\n");
    write(dir.path(), "a.txt", "hit\n");

    let out = SearchTool
        .execute(json!({
            "pattern": "hit",
            "path": dir.path().to_string_lossy(),
            "output_mode": "count",
        }))
        .await
        .expect("search should succeed");

    assert_eq!(out.content, "a.txt:1\nb.txt:2\n");
}

#[tokio::test]
async fn gitignore_excludes_ignored_files() {
    let dir = tempdir().unwrap();
    write(dir.path(), ".gitignore", "ignored/\n");
    write(dir.path(), "ignored/secret.txt", "hello\n");
    write(dir.path(), "visible.txt", "hello\n");

    let out = SearchTool
        .execute(json!({ "pattern": "hello", "path": dir.path().to_string_lossy() }))
        .await
        .expect("search should succeed");

    // The file under an ignored directory must not be searched — this is the
    // whole reason the tool uses the `ignore` walker.
    assert_eq!(out.content, "visible.txt:1:hello\n");
}

#[tokio::test]
async fn long_line_is_truncated_to_a_column_cap() {
    let dir = tempdir().unwrap();
    let long = "a".repeat(500);
    write(dir.path(), "f.txt", &format!("{long}\n"));

    let out = SearchTool
        .execute(json!({ "pattern": "aaa", "path": dir.path().to_string_lossy() }))
        .await
        .expect("search should succeed");

    // Text is capped at 300 columns with an ellipsis so one giant line can't
    // blow the token budget.
    assert_eq!(out.content, format!("f.txt:1:{}…\n", "a".repeat(300)));
}

#[tokio::test]
async fn too_many_matches_are_truncated_with_a_notice() {
    let dir = tempdir().unwrap();
    let body: String = (0..250).map(|_| "x\n").collect();
    write(dir.path(), "many.txt", &body);

    let out = SearchTool
        .execute(json!({ "pattern": "x", "path": dir.path().to_string_lossy() }))
        .await
        .expect("search should succeed");

    let lines: Vec<&str> = out.content.lines().collect();
    // 200 match lines + 1 truncation notice.
    assert_eq!(lines.len(), 201);
    let notice = lines.last().unwrap();
    assert!(notice.contains("250 matches total"), "notice was: {notice}");
    assert!(notice.contains("showing 200"), "notice was: {notice}");
}

#[tokio::test]
async fn binary_file_is_skipped() {
    let dir = tempdir().unwrap();
    // A NUL byte makes this invalid UTF-8; it must be skipped, not crash.
    std::fs::write(dir.path().join("blob.bin"), b"hello\x00\xff\xfeworld").unwrap();
    write(dir.path(), "text.txt", "hello\n");

    let out = SearchTool
        .execute(json!({ "pattern": "hello", "path": dir.path().to_string_lossy() }))
        .await
        .expect("search should succeed");

    assert_eq!(out.content, "text.txt:1:hello\n");
}

#[tokio::test]
async fn invalid_regex_is_an_error() {
    let dir = tempdir().unwrap();
    let err = SearchTool
        .execute(json!({ "pattern": "(unclosed", "path": dir.path().to_string_lossy() }))
        .await
        .expect_err("a malformed pattern should error, not match");
    // The message should be actionable so the model can resend a fixed pattern.
    assert!(
        matches!(&err, ToolError::Other(m) if m.contains("invalid regex")),
        "expected invalid-regex error, got {err:?}"
    );
}
