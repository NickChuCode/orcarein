//! @-mention support: list project files for completion, and expand `@path`
//! tokens to file content blocks at submit time. Pure (no terminal I/O).

use std::path::Path;

/// Recursively list project files from `cwd`, gitignore-aware (same `ignore`
/// walker the search tool uses), `/`-separated relative paths, sorted, capped.
pub fn list_project_files(cwd: &Path, cap: usize) -> Vec<String> {
    let mut builder = ignore::WalkBuilder::new(cwd);
    builder.require_git(false); // honor .gitignore even outside a git repo
    let mut out = Vec::new();
    for result in builder.build() {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(cwd) {
            let s = rel.to_string_lossy().replace('\\', "/");
            if !s.is_empty() {
                out.push(s);
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(cap);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn lists_files_respecting_gitignore_sorted_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "x").unwrap();
        std::fs::write(root.join("b.txt"), "y").unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/junk.rs"), "z").unwrap();
        let mut gi = std::fs::File::create(root.join(".gitignore")).unwrap();
        writeln!(gi, "target/").unwrap();

        let files = list_project_files(root, 100);
        assert!(files.contains(&"src/a.rs".to_string()));
        assert!(files.contains(&"b.txt".to_string()));
        // gitignore honored: no target/ entries
        assert!(!files.iter().any(|f| f.starts_with("target/")));
        // deterministic sort
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted);
        // cap
        assert!(list_project_files(root, 1).len() <= 1);
    }
}
