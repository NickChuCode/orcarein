//! Integration tests for project-memory discovery/loading (v02-20).
use orcarein_core::{find_agents_md, format_memory_block, load_project_memory};
use tempfile::tempdir;

#[test]
fn finds_agents_md_walking_up_from_a_subdir() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "hi").unwrap();
    let sub = dir.path().join("a/b/c");
    std::fs::create_dir_all(&sub).unwrap();

    let found = find_agents_md(&sub).expect("should find root AGENTS.md");
    assert_eq!(found, dir.path().join("AGENTS.md"));
}

#[test]
fn returns_none_when_absent() {
    let dir = tempdir().unwrap();
    assert!(find_agents_md(dir.path()).is_none());
}

#[test]
fn stops_at_nearest_when_nested() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "root").unwrap();
    let sub = dir.path().join("nested");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("AGENTS.md"), "nested").unwrap();

    let found = find_agents_md(&sub).unwrap();
    assert_eq!(found, sub.join("AGENTS.md"));
}

#[test]
fn skips_a_directory_named_agents_md_and_continues_up() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "real file").unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // A *directory* literally named AGENTS.md must be skipped, not matched.
    std::fs::create_dir(sub.join("AGENTS.md")).unwrap();

    let found = find_agents_md(&sub).unwrap();
    assert_eq!(
        found,
        dir.path().join("AGENTS.md"),
        "dir named AGENTS.md must be skipped"
    );
}

#[test]
fn loads_content_untruncated() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "build with cargo").unwrap();
    let mem = load_project_memory(dir.path()).unwrap();
    assert_eq!(mem.content, "build with cargo");
    assert!(!mem.truncated);
}

#[test]
fn empty_or_whitespace_file_is_skipped() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "   \n\t\n").unwrap();
    assert!(load_project_memory(dir.path()).is_none());
}

#[test]
fn truncates_on_a_char_boundary_yielding_valid_utf8() {
    let dir = tempdir().unwrap();
    // 3-byte chars: 32 KiB is not a multiple of 3, so the byte cap lands
    // mid-character. The result must still be valid UTF-8.
    let big = "世".repeat(20_000); // 60_000 bytes > 32 KiB
    std::fs::write(dir.path().join("AGENTS.md"), &big).unwrap();
    let mem = load_project_memory(dir.path()).unwrap();
    assert!(mem.truncated);
    assert!(mem.content.len() <= 32 * 1024);
    // Boundary landed on a whole 3-byte char (would have panicked on a mid-char
    // slice). Pin it explicitly so a buggy boundary walk can't slip through.
    assert_eq!(mem.content.len() % 3, 0);
    assert!(mem.content.chars().all(|c| c == '世'));
}

#[test]
fn format_block_has_delimiter_and_truncation_notice() {
    let plain = format_memory_block("hello", false);
    assert!(plain.contains("# Project context (from AGENTS.md)"));
    assert!(plain.contains("hello"));
    assert!(!plain.contains("truncated"));

    let cut = format_memory_block("hello", true);
    assert!(cut.contains("truncated to 32 KiB"));
}
