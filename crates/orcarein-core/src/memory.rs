//! Project memory — discover and load a repo's `AGENTS.md` so the harness
//! carries project-specific context. Pure, std-only; the binary decides
//! where to inject the formatted block. See the v02-20 design spec.

use std::path::{Path, PathBuf};

/// The project-memory filename (cross-tool convention; other agents read it too).
pub const AGENTS_FILENAME: &str = "AGENTS.md";

/// Cap on injected bytes — bounds context + cost. A larger file is truncated.
pub const MAX_MEMORY_BYTES: usize = 32 * 1024;

/// A loaded project-memory file.
pub struct ProjectMemory {
    pub path: PathBuf,
    pub content: String,
    pub truncated: bool,
}

/// Walk up from `start` to the filesystem root, returning the first
/// `AGENTS.md` that is a regular file. `ancestors()` is purely lexical
/// (it does not follow symlinks), so there is no cycle risk. A *directory*
/// named `AGENTS.md` is skipped and the walk continues upward.
pub fn find_agents_md(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(AGENTS_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Find, read, and bound the project-memory file. Returns `None` when there
/// is no file, it can't be read, or it is empty/whitespace-only (an empty
/// block is pure noise). Oversized content is truncated to the nearest char
/// boundary at or below `MAX_MEMORY_BYTES`.
pub fn load_project_memory(start: &Path) -> Option<ProjectMemory> {
    let path = find_agents_md(start)?;
    let raw = std::fs::read_to_string(&path).ok()?; // unreadable/binary -> skip
    if raw.trim().is_empty() {
        return None;
    }
    let (content, truncated) = if raw.len() > MAX_MEMORY_BYTES {
        let mut i = MAX_MEMORY_BYTES.min(raw.len());
        while i > 0 && !raw.is_char_boundary(i) {
            i -= 1;
        }
        (raw[..i].to_string(), true)
    } else {
        (raw, false)
    };
    Some(ProjectMemory {
        path,
        content,
        truncated,
    })
}

/// Wrap memory content in a delimited block to append to a system prompt.
pub fn format_memory_block(content: &str, truncated: bool) -> String {
    let mut block = format!("\n\n# Project context (from {AGENTS_FILENAME})\n\n{content}\n");
    if truncated {
        block.push_str("(AGENTS.md truncated to 32 KiB)\n");
    }
    block
}
