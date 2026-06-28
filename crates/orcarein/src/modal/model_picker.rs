//! `/model` completion popup state for the modal editor. Parallel to
//! [`crate::modal::mention::MentionState`] but triggered by a line prefix
//! (`/model <arg>`) instead of an `@`; it reuses [`crate::modal::mention::filter`]
//! for ranking. Pure — no ratatui/terminal types; the I/O shell (`mod.rs`) drives
//! it. Always compiled and unit-tested, like `buffer`/`mention`.

use crate::modal::buffer::{Cursor, EditBuffer};

/// The command prefix that arms the picker. The arg starts at `PREFIX.len()`.
const PREFIX: &str = "/model ";

/// Popup state layered over Insert mode while the line is `/model <arg>`.
#[derive(Default)]
pub struct ModelPickerState {
    pub active: bool,
    pub start: Cursor, // first arg column (row 0, col == PREFIX char len)
    pub query: String, // the arg text up to the cursor (whitespace-free)
    pub candidates: Vec<String>,
    pub filtered: Vec<usize>, // indices into `candidates`, best-first
    pub selected: usize,      // index into `filtered`
}

impl ModelPickerState {
    /// Recompute `active`/`start`/`query` from the buffer. Active only on a
    /// single-line `/model <arg>` with the cursor inside the (whitespace-free)
    /// arg — a second line, an `@` mention, or a space in the arg all tear it
    /// down. Returns the new `active`.
    pub fn update_from_buffer(&mut self, buf: &EditBuffer) -> bool {
        let cur = buf.cursor.clone(); // Cursor is Clone, not Copy
        let prefix_len = PREFIX.chars().count();
        let row0: Vec<char> = buf
            .lines
            .first()
            .map(|s| s.chars().collect())
            .unwrap_or_default();
        let line: String = row0.iter().collect();
        let single = buf.lines.len() == 1 && cur.row == 0;
        let has_prefix = line == "/model" || line.starts_with(PREFIX);
        if single && has_prefix && cur.col >= prefix_len && cur.col <= row0.len() {
            let arg: String = row0[prefix_len..cur.col].iter().collect();
            if !arg.chars().any(|c| c.is_whitespace()) {
                self.active = true;
                self.start = Cursor {
                    row: 0,
                    col: prefix_len,
                };
                self.query = arg;
                return true;
            }
        }
        self.active = false;
        false
    }

    /// The edit to apply on accept: `(start, end_col_exclusive, "<id> ")`, where
    /// the half-open span `[start.col, end_col_exclusive)` on row 0 is the typed
    /// arg. The trailing space mirrors `@`-mention so the popup tears down after
    /// accept. `None` when nothing is selectable (empty `filtered`).
    pub fn accept(&self) -> Option<(Cursor, usize, String)> {
        let idx = *self.filtered.get(self.selected)?;
        let id = self.candidates.get(idx)?;
        let end_excl = self.start.col + self.query.chars().count();
        Some((self.start.clone(), end_excl, format!("{id} ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_at(line: &str, col: usize) -> EditBuffer {
        let mut b = EditBuffer::from_str(line);
        b.enter_insert_before();
        b.cursor = Cursor { row: 0, col };
        b
    }

    #[test]
    fn triggers_only_on_single_line_model_prefix() {
        let mut s = ModelPickerState::default();
        // "/model pr" cursor at end (col 9) → active, query "pr".
        assert!(s.update_from_buffer(&buf_at("/model pr", 9)));
        assert_eq!(s.query, "pr");
        assert_eq!(s.start.col, 7);
        // bare "/model " → active, empty query (show all).
        assert!(s.update_from_buffer(&buf_at("/model ", 7)));
        assert_eq!(s.query, "");
        // not a /model line → inactive.
        assert!(!s.update_from_buffer(&buf_at("hello", 5)));
        // arg already has a space (completed) → tears down.
        assert!(!s.update_from_buffer(&buf_at("/model pro ", 11)));
    }

    #[test]
    fn accept_replaces_arg_with_id_plus_space() {
        let mut s = ModelPickerState::default();
        s.update_from_buffer(&buf_at("/model pr", 9));
        s.candidates = vec!["deepseek-v4-pro".to_string()];
        s.filtered = crate::modal::mention::filter(&s.query, &s.candidates);
        s.selected = 0;
        let (at, end_excl, ins) = s.accept().unwrap();
        assert_eq!(at, Cursor { row: 0, col: 7 }); // start of arg
        assert_eq!(end_excl, 9); // cursor col
        assert_eq!(ins, "deepseek-v4-pro "); // id + trailing space
                                             // empty filtered → None
        s.filtered.clear();
        assert!(s.accept().is_none());
    }
}
