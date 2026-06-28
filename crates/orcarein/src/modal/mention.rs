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
    pub at: Cursor,    // the triggering '@' position (char indices)
    pub query: String, // non-whitespace run after '@', up to the cursor
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

impl MentionState {
    /// Recompute `active`/`at`/`query` from the buffer's current row + cursor.
    /// Scans left from the cursor to the nearest `@`; active iff that `@` is at
    /// column 0 or preceded by whitespace, and every char in `(@, cursor]` is
    /// non-whitespace. Returns the new `active`.
    pub fn update_from_buffer(&mut self, buf: &EditBuffer) -> bool {
        let cur = buf.cursor.clone(); // Cursor is Clone, not Copy
        let line: Vec<char> = buf
            .lines
            .get(cur.row)
            .map(|s| s.chars().collect())
            .unwrap_or_default();
        let mut i = cur.col.min(line.len());
        let mut at = None;
        while i > 0 {
            i -= 1;
            let c = line[i];
            if c == '@' {
                at = Some(i);
                break;
            }
            if c.is_whitespace() {
                break; // hit whitespace before any '@' → no mention
            }
        }
        match at {
            Some(a) if a == 0 || line[a - 1].is_whitespace() => {
                self.active = true;
                self.at = Cursor {
                    row: cur.row,
                    col: a,
                };
                self.query = line[a + 1..cur.col.min(line.len())].iter().collect();
                true
            }
            _ => {
                self.active = false;
                false
            }
        }
    }

    /// The edit to apply on accept: `(at, end_col_exclusive, "@<path> ")`, where
    /// the half-open span `[at.col, end_col_exclusive)` on `at.row` is the typed
    /// `@query`. `None` when nothing is selectable (empty `filtered`).
    pub fn accept(&self) -> Option<(Cursor, usize, String)> {
        let idx = *self.filtered.get(self.selected)?;
        let path = self.candidates.get(idx)?;
        let end_excl = self.at.col + 1 + self.query.chars().count();
        Some((self.at.clone(), end_excl, format!("@{path} "))) // Cursor: Clone, not Copy
    }
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

    fn buf_at(line: &str, col: usize) -> EditBuffer {
        let mut b = EditBuffer::from_str(line);
        b.enter_insert_before();
        b.cursor = Cursor { row: 0, col };
        b
    }

    #[test]
    fn update_triggers_on_boundary_at_and_extracts_query() {
        let mut m = MentionState::default();
        // "@mar" with cursor at end (col 4): @ at col 0 (line start) → active.
        let b = buf_at("@mar", 4);
        assert!(m.update_from_buffer(&b));
        assert_eq!(m.query, "mar");
        assert_eq!(m.at.col, 0);

        // "see @mo" cursor at 7: @ at col 4, preceded by space → active.
        let b = buf_at("see @mo", 7);
        assert!(m.update_from_buffer(&b));
        assert_eq!(m.query, "mo");
        assert_eq!(m.at.col, 4);

        // email "a@b" cursor at 3: @ preceded by 'a' (non-space) → inactive.
        let b = buf_at("a@b", 3);
        assert!(!m.update_from_buffer(&b));

        // whitespace between @ and cursor tears down: "@mo x" cursor at 5.
        let b = buf_at("@mo x", 5);
        assert!(!m.update_from_buffer(&b));
    }

    #[test]
    fn accept_returns_replacement_span_and_string() {
        let mut m = MentionState {
            active: true,
            at: Cursor { row: 0, col: 4 }, // '@' at col 4
            query: "mo".to_string(),       // "@mo" → cursor at col 7
            candidates: vec!["src/modal/mod.rs".to_string()],
            filtered: vec![0],
            selected: 0,
        };
        let (at, end_excl, ins) = m.accept().unwrap();
        assert_eq!(at, Cursor { row: 0, col: 4 });
        assert_eq!(end_excl, 7); // at.col(4) + 1('@') + query len(2)
        assert_eq!(ins, "@src/modal/mod.rs ");
        // empty filtered → None
        m.filtered.clear();
        assert!(m.accept().is_none());
    }
}
