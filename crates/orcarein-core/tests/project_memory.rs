//! Integration tests for project-memory discovery/loading (v02-20).
use orcarein_core::find_agents_md;
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
