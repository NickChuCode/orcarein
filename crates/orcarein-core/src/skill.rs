//! Skills — named, on-demand instruction packs a repo carries in
//! `.orcarein/skills/`. Pure, std-only, and SILENT (mirrors `memory.rs`:
//! unreadable / malformed files are skipped, never logged — core has no
//! `tracing`). The binary discovers skills once at startup, injects a compact
//! index into the system prompt, and registers the `skill` tool that loads a
//! body on demand. See the 2026-06-30 skill design spec.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The skills directory, relative to a repo root (Claude Code / superpowers
/// convention). `/` works as a separator on Windows too via `Path::join`.
pub const SKILLS_DIRNAME: &str = ".orcarein/skills";

/// Cap on a single skill body returned to the model (same bound as AGENTS.md).
pub const MAX_SKILL_BODY_BYTES: usize = 32 * 1024;

/// Cap on how many skills feed the index — bounds the stable-prefix size.
pub const MAX_SKILLS: usize = 64;

/// A discovered skill: `name`/`description` from frontmatter, `body` = the
/// markdown after the closing `---` fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Strips one matching pair of surrounding quotes (`"` or `'`), if present.
fn strip_quotes(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse one skill file. Requires: the first non-blank line is a `---` fence;
/// `name:`/`description:` key lines inside; a closing `---`; then the body.
/// Only `name`/`description` are recognized (other keys ignored). `name` is
/// required — a missing/empty name (or no frontmatter / no closing fence)
/// yields `None` (the file is skipped). CRLF-tolerant. The body is sliced
/// **verbatim from the original `content`** (never rebuilt from `.lines()`,
/// which would lose CRLF); line scanning only locates the closing fence.
pub fn parse_skill(content: &str) -> Option<Skill> {
    // Strip a leading UTF-8 BOM: PowerShell's `Set-Content`/`Out-File -Encoding
    // utf8` (PS 5.1) writes one, and it would otherwise make the first line
    // `\u{FEFF}---` != `---`, silently skipping the whole file. Rebinding here
    // keeps all downstream byte offsets consistent with the sliced body.
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(content);

    let mut name: Option<String> = None;
    let mut description = String::new();
    let mut seen_open = false;
    let mut body_start: Option<usize> = None;
    let mut offset = 0usize; // byte offset of the current line's start

    for line in content.split_inclusive('\n') {
        let line_len = line.len();
        let t = line.trim_end_matches('\n').trim_end_matches('\r').trim();

        if !seen_open {
            if t.is_empty() {
                offset += line_len;
                continue; // skip leading blank lines
            }
            if t == "---" {
                seen_open = true;
                offset += line_len;
                continue;
            }
            return None; // first non-blank line is not a fence -> not a skill
        }

        if t == "---" {
            body_start = Some(offset + line_len); // body begins after this line
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "name" => name = Some(strip_quotes(v.trim()).to_string()),
                "description" => description = strip_quotes(v.trim()).to_string(),
                _ => {}
            }
        }
        offset += line_len;
    }

    let body_start = body_start?; // no closing fence -> skip
    let name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())?;

    let rest = &content[body_start..];
    let body = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest)
        .to_string();

    Some(Skill {
        name,
        description,
        body,
    })
}

/// Cap on a description's rendered length (chars) — keeps index lines compact.
const MAX_DESC_CHARS: usize = 200;

/// Collapse internal whitespace/newlines to single spaces and cap the length.
fn one_line(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_DESC_CHARS {
        let head: String = collapsed.chars().take(MAX_DESC_CHARS).collect();
        format!("{head}…")
    } else {
        collapsed
    }
}

/// Model-facing index: goes into the system prompt's stable prefix. Only
/// name + description — never a body (that's the on-demand property). The
/// caller uses this ONLY when `skills` is non-empty.
pub fn skills_index(skills: &[Skill]) -> String {
    let mut s =
        String::from("# Available skills (call the `skill` tool with a name to load one)\n");
    for sk in skills {
        s.push_str("- ");
        s.push_str(&sk.name);
        s.push_str(": ");
        s.push_str(&one_line(&sk.description));
        s.push('\n');
    }
    s
}

/// Human-facing list for the `/skills` command (no "call the tool" header).
/// An empty slice yields an empty string; the binary prints an empty-state hint.
pub fn skills_list(skills: &[Skill]) -> String {
    let mut s = String::new();
    for sk in skills {
        s.push_str("- ");
        s.push_str(&sk.name);
        s.push_str(" — ");
        s.push_str(&one_line(&sk.description));
        s.push('\n');
    }
    s
}

/// Walk up from `start` to the filesystem root; return the first existing
/// `<ancestor>/.orcarein/skills` directory. Purely lexical (`ancestors()`),
/// so no symlink cycle risk — same shape as `memory::find_agents_md`.
pub fn find_skills_dir(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(SKILLS_DIRNAME);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Discover every skill under the nearest `.orcarein/skills/`, from BOTH
/// layouts: flat `*.md` files and nested `<name>/SKILL.md` (one level deep).
/// Malformed / unreadable files are skipped silently. Candidates are sorted by
/// path for determinism, deduped by frontmatter `name` (first-by-path wins),
/// then returned sorted by `name` and capped at `MAX_SKILLS`. The deterministic
/// order is required so the injected index is byte-stable (cache prefix).
pub fn discover_skills(start: &Path) -> Vec<Skill> {
    let Some(dir) = find_skills_dir(start) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new(); // permission / race -> nothing, silently
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let is_md = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("md"));
            if is_md {
                candidates.push(path);
            }
        } else if path.is_dir() {
            // Nested layout: <name>/SKILL.md (canonical superpowers/Claude Code
            // name). One level only — no recursion.
            let nested = path.join("SKILL.md");
            if nested.is_file() {
                candidates.push(nested);
            }
        }
    }
    candidates.sort();

    let mut by_name: BTreeMap<String, Skill> = BTreeMap::new();
    for path in candidates {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(skill) = parse_skill(&raw) {
            by_name.entry(skill.name.clone()).or_insert(skill);
        }
    }
    by_name.into_values().take(MAX_SKILLS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk(name: &str, desc: &str) -> Skill {
        Skill {
            name: name.into(),
            description: desc.into(),
            body: "BODY".into(),
        }
    }

    #[test]
    fn index_is_golden_and_carries_no_body() {
        let skills = [
            sk("release", "how to cut a release"),
            sk("triage", "how we label issues"),
        ];
        let idx = skills_index(&skills);
        assert_eq!(
            idx,
            "# Available skills (call the `skill` tool with a name to load one)\n\
             - release: how to cut a release\n\
             - triage: how we label issues\n"
        );
        // The defining "on-demand" property: no body text in the index.
        assert!(!idx.contains("BODY"));
    }

    #[test]
    fn index_single_lines_multiline_description() {
        let skills = [sk("x", "line one\nline two   with   spaces")];
        let idx = skills_index(&skills);
        assert!(idx.contains("- x: line one line two with spaces\n"));
    }

    #[test]
    fn list_is_golden_and_empty_slice_is_empty() {
        let skills = [sk("release", "cut a release")];
        assert_eq!(skills_list(&skills), "- release — cut a release\n");
        assert_eq!(skills_list(&[]), "");
    }

    #[test]
    fn parses_valid_skill() {
        let s = "---\nname: release\ndescription: how to cut a release\n---\nStep 1.\nStep 2.\n";
        let sk = parse_skill(s).expect("should parse");
        assert_eq!(sk.name, "release");
        assert_eq!(sk.description, "how to cut a release");
        assert_eq!(sk.body, "Step 1.\nStep 2.\n");
    }

    #[test]
    fn missing_name_is_none() {
        let s = "---\ndescription: no name here\n---\nbody\n";
        assert!(parse_skill(s).is_none());
    }

    #[test]
    fn missing_description_defaults_empty() {
        let s = "---\nname: solo\n---\nbody\n";
        let sk = parse_skill(s).unwrap();
        assert_eq!(sk.name, "solo");
        assert_eq!(sk.description, "");
    }

    #[test]
    fn no_opening_fence_is_none() {
        assert!(parse_skill("name: x\ndescription: y\nbody\n").is_none());
    }

    #[test]
    fn no_closing_fence_is_none() {
        assert!(parse_skill("---\nname: x\ndescription: y\nbody\n").is_none());
    }

    #[test]
    fn body_is_verbatim_after_fence() {
        let s = "---\nname: code\n---\n```rust\nfn main() {}\n```\n\nmore\n";
        let sk = parse_skill(s).unwrap();
        assert_eq!(sk.body, "```rust\nfn main() {}\n```\n\nmore\n");
    }

    #[test]
    fn crlf_is_tolerated_and_body_preserves_crlf() {
        let s = "---\r\nname: win\r\ndescription: crlf\r\n---\r\nline1\r\nline2\r\n";
        let sk = parse_skill(s).unwrap();
        assert_eq!(sk.name, "win");
        assert_eq!(sk.description, "crlf");
        assert_eq!(sk.body, "line1\r\nline2\r\n");
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let s = "---\nname: \"quoted\"\ndescription: 'single'\n---\nb\n";
        let sk = parse_skill(s).unwrap();
        assert_eq!(sk.name, "quoted");
        assert_eq!(sk.description, "single");
    }

    #[test]
    fn leading_utf8_bom_is_tolerated() {
        // PowerShell's utf8 encoders prepend a BOM; without stripping it the
        // opening fence would not match and the file would be silently skipped.
        let s = "\u{FEFF}---\nname: bom\ndescription: d\n---\nbody\n";
        let sk = parse_skill(s).expect("BOM-prefixed skill should parse");
        assert_eq!(sk.name, "bom");
        assert_eq!(sk.body, "body\n");
    }

    use std::fs;
    use tempfile::tempdir;

    fn write(path: &std::path::Path, content: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn valid(name: &str) -> String {
        format!("---\nname: {name}\ndescription: d {name}\n---\nbody {name}\n")
    }

    #[test]
    fn discovers_flat_md_sorted_by_name() {
        let dir = tempdir().unwrap();
        let sk = dir.path().join(".orcarein/skills");
        write(&sk.join("b.md"), &valid("bravo"));
        write(&sk.join("a.md"), &valid("alpha"));
        let skills = discover_skills(dir.path());
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "bravo"]
        );
    }

    #[test]
    fn skips_malformed_and_non_md_and_bad_subdirs() {
        let dir = tempdir().unwrap();
        let sk = dir.path().join(".orcarein/skills");
        write(&sk.join("good.md"), &valid("good"));
        write(&sk.join("bad.md"), "no frontmatter here\n"); // -> skipped
        write(&sk.join("notes.txt"), &valid("ignored")); // wrong ext
        write(&sk.join("emptydir/README.md"), &valid("nope")); // subdir w/o SKILL.md
        let skills = discover_skills(dir.path());
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["good"]
        );
    }

    #[test]
    fn discovers_nested_skill_md_with_frontmatter_name() {
        let dir = tempdir().unwrap();
        let sk = dir.path().join(".orcarein/skills");
        // Directory named "rel" but frontmatter name is authoritative.
        write(&sk.join("rel/SKILL.md"), &valid("release"));
        let skills = discover_skills(dir.path());
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["release"]
        );
    }

    #[test]
    fn walks_up_to_find_skills_dir_from_deeper_cwd() {
        let dir = tempdir().unwrap();
        let sk = dir.path().join(".orcarein/skills");
        write(&sk.join("a.md"), &valid("alpha"));
        let deep = dir.path().join("src/sub");
        fs::create_dir_all(&deep).unwrap();
        let skills = discover_skills(&deep);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "alpha");
    }

    #[test]
    fn absent_dir_yields_empty() {
        let dir = tempdir().unwrap();
        assert!(discover_skills(dir.path()).is_empty());
    }

    #[test]
    fn duplicate_name_first_by_sorted_path_wins() {
        let dir = tempdir().unwrap();
        let sk = dir.path().join(".orcarein/skills");
        // Both declare name "dup"; "a.md" sorts before "b.md" -> a wins.
        write(
            &sk.join("a.md"),
            "---\nname: dup\ndescription: from a\n---\nA\n",
        );
        write(
            &sk.join("b.md"),
            "---\nname: dup\ndescription: from b\n---\nB\n",
        );
        let skills = discover_skills(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "from a");
    }
}
