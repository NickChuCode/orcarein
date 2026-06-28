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

/// Scan `text` for `@<path>` tokens (path = non-whitespace run after a boundary
/// `@` — start-of-text or whitespace, which covers `\n`). For each path that
/// `reader` resolves, append a delimited `<file>` block; leave unresolved ones
/// as literal text. Dedupe by path; the user's text is preserved verbatim.
pub fn expand_mentions(text: &str, reader: impl Fn(&str) -> Option<String>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut paths: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut i = 0;
    while i < n {
        let boundary = i == 0 || chars[i - 1].is_whitespace();
        if chars[i] == '@' && boundary {
            let mut j = i + 1;
            while j < n && !chars[j].is_whitespace() {
                j += 1;
            }
            let mut raw: String = chars[i + 1..j].iter().collect();
            while raw.ends_with(['.', ',', ';', ':', ')']) {
                raw.pop();
            }
            if !raw.is_empty() && seen.insert(raw.clone()) {
                paths.push(raw);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let mut blocks = String::new();
    for p in &paths {
        if let Some(content) = reader(p) {
            blocks.push_str(&format!("\n\n<file path=\"{p}\">\n{content}\n</file>"));
        }
    }
    if blocks.is_empty() {
        text.to_string()
    } else {
        format!("{text}{blocks}")
    }
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

    #[test]
    fn expands_resolved_mentions_dedup_and_leaves_unresolved() {
        let reader = |p: &str| match p {
            "src/a.rs" => Some("CONTENT_A".to_string()),
            _ => None,
        };
        // single mention → file block appended, original text kept
        let out = expand_mentions("see @src/a.rs please", reader);
        assert!(out.starts_with("see @src/a.rs please"));
        assert!(out.contains("<file path=\"src/a.rs\">"));
        assert!(out.contains("CONTENT_A"));
        // dedupe: two of the same path → one block
        let out2 = expand_mentions("@src/a.rs @src/a.rs", reader);
        assert_eq!(out2.matches("<file path=").count(), 1);
        // unresolved mention → left as literal, no block
        let out3 = expand_mentions("@missing.rs", reader);
        assert_eq!(out3, "@missing.rs");
        // email a@b is NOT a mention (no boundary before @)
        let out4 = expand_mentions("mail a@src/a.rs", reader);
        assert!(!out4.contains("<file"));
        // no mention → unchanged
        assert_eq!(expand_mentions("plain text", reader), "plain text");
    }
}
