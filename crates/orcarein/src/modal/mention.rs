//! @-mention popup state for the modal editor: which project files match the
//! current `@query`, the selected row, and the edit to apply on accept. Pure —
//! no ratatui/terminal types; the I/O shell (mod.rs) drives it. Always compiled
//! and unit-tested, like buffer/command/render.

use crate::modal::buffer::{Cursor, EditBuffer};

/// Popup state layered over Insert mode. `active` only in Insert (set by
/// `update_from_buffer`).
#[derive(Default)]
pub struct MentionState {
    pub active: bool,
    pub at: Cursor,           // the triggering '@' position (char indices)
    pub query: String,        // non-whitespace run after '@', up to the cursor
    pub candidates: Vec<String>,
    pub filtered: Vec<usize>, // indices into `candidates`, best-first
    pub selected: usize,      // index into `filtered`
}

/// Case-insensitive subsequence match; returns `(not_substring, span, path_len)`
/// as a sort key (smaller = better) or `None` if `query` isn't a subsequence.
fn score(query_lc: &str, cand: &str) -> Option<(u8, usize, usize)> {
    let cand_lc = cand.to_lowercase();
    if query_lc.is_empty() {
        return Some((0, 0, cand.chars().count()));
    }
    let cand_chars: Vec<char> = cand_lc.chars().collect();
    let q: Vec<char> = query_lc.chars().collect();
    let mut qi = 0;
    let mut first = None;
    let mut last = 0;
    for (ci, &cc) in cand_chars.iter().enumerate() {
        if qi < q.len() && cc == q[qi] {
            if first.is_none() {
                first = Some(ci);
            }
            last = ci;
            qi += 1;
        }
    }
    if qi < q.len() {
        return None; // not a subsequence
    }
    let span = last - first.unwrap() + 1;
    let not_substring = if cand_lc.contains(query_lc) { 0 } else { 1 };
    Some((not_substring, span, cand.chars().count()))
}

/// Filter `candidates` to those matching `query` (subsequence, case-insensitive),
/// best-first; ties broken lexicographically for determinism. Empty query → all,
/// in candidate order.
pub fn filter(query: &str, candidates: &[String]) -> Vec<usize> {
    if query.is_empty() {
        // Empty query → all candidates in their (already deterministic) order.
        return (0..candidates.len()).collect();
    }
    let q = query.to_lowercase();
    let mut scored: Vec<(usize, (u8, usize, usize))> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| score(&q, c).map(|s| (i, s)))
        .collect();
    // Best-first; lexicographic tiebreak on the path for determinism.
    scored.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| candidates[a.0].cmp(&candidates[b.0]))
    });
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands() -> Vec<String> {
        vec![
            "src/main.rs".to_string(),
            "src/markdown.rs".to_string(),
            "src/modal/mod.rs".to_string(),
            "README.md".to_string(),
        ]
    }

    #[test]
    fn filter_subsequence_ranks_and_is_deterministic() {
        let c = cands();
        // "mar" is a substring of markdown.rs and a subsequence of others.
        let got: Vec<&str> = filter("mar", &c).iter().map(|&i| c[i].as_str()).collect();
        assert_eq!(got.first(), Some(&"src/markdown.rs")); // substring wins
        // no match
        assert!(filter("zzz", &c).is_empty());
        // empty query → all, candidate order
        assert_eq!(filter("", &c), vec![0, 1, 2, 3]);
        // case-insensitive
        assert!(!filter("README", &c).is_empty());
    }
}
