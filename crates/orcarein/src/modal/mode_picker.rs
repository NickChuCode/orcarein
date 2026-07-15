//! `/mode` completion popup state for the modal editor. Parallel to
//! [`crate::modal::model_picker::ModelPickerState`], but its candidate set is a
//! fixed list of the four permission modes instead of a live model list, so it
//! carries its own candidates and needs no external source. Pure — no
//! ratatui/terminal types; the I/O shell (`mod.rs`) drives it. Always compiled
//! and unit-tested, like `buffer`/`mention`/`model_picker`.

use crate::modal::buffer::{Cursor, EditBuffer};

/// The command prefix that arms the picker. The arg starts at `PREFIX.len()`.
const PREFIX: &str = "/mode ";

/// The four selectable permission modes, in canonical order.
const MODES: [&str; 4] = ["default", "acceptEdits", "plan", "yolo"];

/// Popup state layered over Insert mode while the line is `/mode <arg>`.
///
/// `candidates` is pre-filled with [`MODES`] and never changes (unlike the
/// model picker, whose list is fetched at runtime), so the driver only refreshes
/// `active`/`query`/`filtered`.
pub struct ModePickerState {
    pub active: bool,
    pub start: Cursor, // first arg column (row 0, col == PREFIX char len)
    pub query: String, // the arg text up to the cursor (whitespace-free)
    pub candidates: Vec<String>,
    pub filtered: Vec<usize>, // indices into `candidates`, best-first
    pub selected: usize,      // index into `filtered`
}

impl Default for ModePickerState {
    fn default() -> Self {
        ModePickerState {
            active: false,
            start: Cursor::default(),
            query: String::new(),
            candidates: MODES.iter().map(|s| s.to_string()).collect(),
            filtered: Vec::new(),
            selected: 0,
        }
    }
}

impl ModePickerState {
    /// Recompute `active`/`start`/`query` from the buffer. Active only on a
    /// single-line `/mode <arg>` with the cursor inside the (whitespace-free)
    /// arg — a second line or a space in the arg tears it down. The trailing
    /// space in `PREFIX` means the sibling `/model` command never arms this
    /// picker in the course of normal typing. Returns the new `active`.
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
        let has_prefix = line == "/mode" || line.starts_with(PREFIX);
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

    /// The edit to apply on accept: `(start, end_col_exclusive, "<mode> ")`,
    /// where the half-open span `[start.col, end_col_exclusive)` on row 0 is the
    /// typed arg. The trailing space mirrors the model picker so the popup tears
    /// down after accept. `None` when nothing is selectable (empty `filtered`).
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
    fn triggers_only_on_single_line_mode_prefix() {
        let mut s = ModePickerState::default();
        // "/mode pl" cursor at end (col 8) → active, query "pl", arg starts col 6.
        assert!(s.update_from_buffer(&buf_at("/mode pl", 8)));
        assert_eq!(s.query, "pl");
        assert_eq!(s.start.col, 6);
        // bare "/mode " → active, empty query (show all four).
        assert!(s.update_from_buffer(&buf_at("/mode ", 6)));
        assert_eq!(s.query, "");
        // not a /mode line → inactive.
        assert!(!s.update_from_buffer(&buf_at("hello", 5)));
        // arg already has a space (completed) → tears down.
        assert!(!s.update_from_buffer(&buf_at("/mode plan ", 11)));
    }

    #[test]
    fn sibling_model_command_does_not_arm() {
        // Disjointness: `/model` and its intermediate `/mode` (no trailing
        // space) must NOT arm the mode picker — else typing /model would flicker.
        let mut s = ModePickerState::default();
        assert!(!s.update_from_buffer(&buf_at("/model", 6)));
        assert!(!s.update_from_buffer(&buf_at("/mode", 5)));
    }

    #[test]
    fn candidates_are_the_four_modes() {
        let s = ModePickerState::default();
        assert_eq!(s.candidates, ["default", "acceptEdits", "plan", "yolo"]);
    }

    #[test]
    fn accept_replaces_arg_with_mode_plus_space() {
        let mut s = ModePickerState::default();
        s.update_from_buffer(&buf_at("/mode pl", 8));
        s.filtered = crate::modal::mention::filter(&s.query, &s.candidates);
        s.selected = 0;
        let (at, end_excl, ins) = s.accept().unwrap();
        assert_eq!(at, Cursor { row: 0, col: 6 }); // start of arg
        assert_eq!(end_excl, 8); // cursor col
        assert_eq!(ins, "plan "); // mode + trailing space
                                  // empty filtered → None
        s.filtered.clear();
        assert!(s.accept().is_none());
    }
}
