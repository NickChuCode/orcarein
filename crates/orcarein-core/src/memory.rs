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
