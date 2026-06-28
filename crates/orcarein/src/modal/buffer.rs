//! Pure multiline edit buffer + vim modal state. No terminal I/O — fully
//! unit-tested. Three-layer index separation (spec §4): storage by byte
//! (String), edit/cursor by char index (Cursor.col), display by width
//! (computed in render via header::disp_width). See the vim-modal spec.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize, // char index within the row (NOT byte, NOT display col)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Visual(VisualKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualKind {
    Char,
    Line,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Register {
    pub text: String,
    pub linewise: bool, // true when produced by yy/dd/Y/V-yank
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Cursor,
}

#[derive(Debug, Clone)]
pub struct EditBuffer {
    pub lines: Vec<String>,
    pub cursor: Cursor,
    pub mode: Mode,
    pub anchor: Option<Cursor>,
    pub register: Register,
    pub desired_col: usize,
    pub scroll: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl EditBuffer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Cursor::default(),
            mode: Mode::Normal,
            anchor: None,
            register: Register::default(),
            desired_col: 0,
            scroll: 0,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn from_str(s: &str) -> Self {
        let mut b = Self::new();
        b.lines = if s.is_empty() {
            vec![String::new()]
        } else {
            s.split('\n').map(|l| l.to_string()).collect()
        };
        b
    }

    /// Buffer contents joined with '\n' — what gets submitted.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Char count of a row (the edit-domain length).
    fn row_chars(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |l| l.chars().count())
    }

    /// Force cursor into a legal position for the current mode (spec §4 invariant).
    pub fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        let last_row = self.lines.len() - 1;
        if self.cursor.row > last_row {
            self.cursor.row = last_row;
        }
        let n = self.row_chars(self.cursor.row);
        // Insert mode may sit one past the last char; Normal/Visual cap at last char.
        let max = match self.mode {
            Mode::Insert => n,
            _ => n.saturating_sub(1).min(n), // empty line -> 0
        };
        if self.cursor.col > max {
            self.cursor.col = max;
        }
    }

    // ---- Cursor motions (shared by Normal/Visual). All char-index based. ----

    /// `h` — left one char (saturating).
    pub fn move_h(&mut self) {
        self.cursor.col = self.cursor.col.saturating_sub(1);
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    /// `l` — right one char (clamp handles upper bound).
    pub fn move_l(&mut self) {
        self.cursor.col += 1;
        self.clamp_cursor();
        self.desired_col = self.cursor.col;
    }

    /// `j` — down one row, restoring `desired_col` (clamped to the new row).
    pub fn move_j(&mut self) {
        // The cursor's current column becomes the sticky goal if it sits
        // further right than the remembered one (e.g. after a direct seek).
        self.desired_col = self.desired_col.max(self.cursor.col);
        let last_row = self.lines.len().saturating_sub(1);
        if self.cursor.row < last_row {
            self.cursor.row += 1;
        }
        self.cursor.col = self.desired_col;
        self.clamp_cursor();
    }

    /// `k` — up one row, restoring `desired_col` (clamped to the new row).
    pub fn move_k(&mut self) {
        self.desired_col = self.desired_col.max(self.cursor.col);
        self.cursor.row = self.cursor.row.saturating_sub(1);
        self.cursor.col = self.desired_col;
        self.clamp_cursor();
    }

    /// `0` — first column of the row.
    pub fn move_line_start(&mut self) {
        self.cursor.col = 0;
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    /// `$` — last char of the row (clamp caps at last char index in Normal/Visual).
    pub fn move_line_end(&mut self) {
        self.cursor.col = self.row_chars(self.cursor.row);
        self.clamp_cursor();
        self.desired_col = self.cursor.col;
    }

    /// `^` — first non-whitespace char of the row (0 if all blank/empty).
    pub fn move_first_nonblank(&mut self) {
        let row = self.cursor.row;
        let col = self
            .lines
            .get(row)
            .and_then(|l| l.chars().position(|c| !c.is_whitespace()))
            .unwrap_or(0);
        self.cursor.col = col;
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    /// `gg` — first row, column 0.
    pub fn move_buffer_top(&mut self) {
        self.cursor.row = 0;
        self.cursor.col = 0;
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    /// `G` — last row, column 0.
    pub fn move_buffer_bottom(&mut self) {
        self.cursor.row = self.lines.len().saturating_sub(1);
        self.cursor.col = 0;
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    /// `w` — start of the next whitespace-delimited word (MVP: no punctuation
    /// word classes). Scans the current row only; if no further word, lands on
    /// the last char.
    pub fn move_word_forward(&mut self) {
        let chars: Vec<char> = self
            .lines
            .get(self.cursor.row)
            .map_or_else(Vec::new, |l| l.chars().collect());
        let n = chars.len();
        let mut i = self.cursor.col;
        // Skip the current word's remaining non-space chars.
        while i < n && !chars[i].is_whitespace() {
            i += 1;
        }
        // Skip the gap of whitespace.
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        // If we ran off the end, stay on the last char.
        self.cursor.col = if i < n { i } else { n.saturating_sub(1) };
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    /// `b` — start of the previous whitespace-delimited word (MVP).
    pub fn move_word_back(&mut self) {
        let chars: Vec<char> = self
            .lines
            .get(self.cursor.row)
            .map_or_else(Vec::new, |l| l.chars().collect());
        if self.cursor.col == 0 {
            self.desired_col = 0;
            self.clamp_cursor();
            return;
        }
        let mut i = self.cursor.col - 1;
        // Skip whitespace to the left.
        while i > 0 && chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i -= 1;
        }
        // Move to the start of this word.
        while i > 0 && chars.get(i - 1).is_some_and(|c| !c.is_whitespace()) {
            i -= 1;
        }
        self.cursor.col = i;
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    /// `e` — end (last char) of the next whitespace-delimited word (MVP).
    pub fn move_word_end(&mut self) {
        let chars: Vec<char> = self
            .lines
            .get(self.cursor.row)
            .map_or_else(Vec::new, |l| l.chars().collect());
        let n = chars.len();
        let mut i = self.cursor.col + 1;
        // Skip whitespace to reach the next word.
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        // Advance to the last non-space char of that word.
        while i + 1 < n && !chars[i + 1].is_whitespace() {
            i += 1;
        }
        self.cursor.col = if i < n { i } else { n.saturating_sub(1) };
        self.desired_col = self.cursor.col;
        self.clamp_cursor();
    }

    // ---- Undo bookkeeping (real, not a stub). redo() comes in Task 7. ----

    /// Snapshot the current state before a mutation and invalidate redo.
    fn push_undo(&mut self) {
        self.undo.push(Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor.clone(),
        });
        self.redo.clear();
    }

    // ---- Mode entry (NOT mutations — no undo push). ----

    /// `i` — insert before the cursor (no movement).
    pub fn enter_insert_before(&mut self) {
        self.mode = Mode::Insert;
        self.clamp_cursor();
    }

    /// `a` — insert after the cursor (col + 1, may sit one past last char).
    pub fn enter_insert_after(&mut self) {
        self.mode = Mode::Insert;
        self.cursor.col += 1;
        self.clamp_cursor();
    }

    /// `I` — insert at the first non-blank char of the row (vim-correct).
    pub fn enter_insert_line_start(&mut self) {
        self.move_first_nonblank();
        self.mode = Mode::Insert;
        self.clamp_cursor();
    }

    /// `A` — insert at the end of the row (col = char count, past last char).
    pub fn enter_insert_line_end(&mut self) {
        self.mode = Mode::Insert;
        self.cursor.col = self.row_chars(self.cursor.row);
        self.clamp_cursor();
    }

    /// `o` — open a blank line below the current row and enter insert.
    pub fn open_below(&mut self) {
        self.push_undo();
        let row = self.cursor.row + 1;
        self.lines.insert(row, String::new());
        self.cursor = Cursor { row, col: 0 };
        self.mode = Mode::Insert;
        self.clamp_cursor();
    }

    /// `O` — open a blank line above the current row and enter insert.
    pub fn open_above(&mut self) {
        self.push_undo();
        let row = self.cursor.row;
        self.lines.insert(row, String::new());
        self.cursor = Cursor { row, col: 0 };
        self.mode = Mode::Insert;
        self.clamp_cursor();
    }

    /// `Esc` — leave insert mode. Set Normal, then clamp (which steps the
    /// cursor back off the past-the-end position, matching vim).
    pub fn leave_insert(&mut self) {
        self.mode = Mode::Normal;
        self.clamp_cursor();
    }

    // ---- Insert-mode editing (char-boundary safe, CJK-safe). ----

    /// Byte offset of char index `col` within `line` (end-of-line if past it).
    fn byte_at(line: &str, col: usize) -> usize {
        line.char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(line.len())
    }

    /// Insert one char at the cursor, advancing the cursor by one char.
    pub fn insert_char(&mut self, c: char) {
        self.push_undo();
        let line = &mut self.lines[self.cursor.row];
        let at = Self::byte_at(line, self.cursor.col);
        line.insert(at, c);
        self.cursor.col += 1;
        self.desired_col = self.cursor.col;
    }

    /// Split the current line at the cursor; cursor drops to (row + 1, col 0).
    pub fn insert_newline(&mut self) {
        self.push_undo();
        let row = self.cursor.row;
        let at = Self::byte_at(&self.lines[row], self.cursor.col);
        let tail = self.lines[row].split_off(at);
        self.lines.insert(row + 1, tail);
        self.cursor = Cursor {
            row: row + 1,
            col: 0,
        };
        self.desired_col = 0;
    }

    /// Delete the char before the cursor; at col 0 join onto the previous line.
    pub fn backspace(&mut self) {
        if self.cursor.col == 0 && self.cursor.row == 0 {
            return; // top-left: nothing to delete
        }
        self.push_undo();
        if self.cursor.col > 0 {
            let line = &mut self.lines[self.cursor.row];
            let start = Self::byte_at(line, self.cursor.col - 1);
            line.remove(start);
            self.cursor.col -= 1;
        } else {
            // col == 0, row > 0: join this line onto the end of the previous.
            let row = self.cursor.row;
            let prev_len = self.row_chars(row - 1);
            let cur = self.lines.remove(row);
            self.lines[row - 1].push_str(&cur);
            self.cursor = Cursor {
                row: row - 1,
                col: prev_len,
            };
        }
        self.desired_col = self.cursor.col;
    }
}

impl Default for EditBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_has_one_empty_line_cursor_origin_normal() {
        let b = EditBuffer::new();
        assert_eq!(b.lines, vec![String::new()]);
        assert_eq!(b.cursor, Cursor { row: 0, col: 0 });
        assert!(matches!(b.mode, Mode::Normal));
    }

    #[test]
    fn from_str_splits_lines_and_is_never_empty() {
        let b = EditBuffer::from_str("ab\ncd");
        assert_eq!(b.lines, vec!["ab".to_string(), "cd".to_string()]);
        let e = EditBuffer::from_str("");
        assert_eq!(e.lines, vec![String::new()]); // invariant: non-empty
    }

    #[test]
    fn text_joins_lines_with_newline() {
        assert_eq!(EditBuffer::from_str("a\nb").text(), "a\nb");
    }

    #[test]
    fn clamp_cursor_keeps_row_and_col_in_range() {
        let mut b = EditBuffer::from_str("abc");
        b.cursor = Cursor { row: 9, col: 9 };
        b.clamp_cursor();
        // Normal mode: col <= last char index (len-1) = 2.
        assert_eq!(b.cursor, Cursor { row: 0, col: 2 });
    }

    #[test]
    fn hjkl_moves_and_clamps() {
        let mut b = EditBuffer::from_str("abc\nde");
        b.move_l();
        assert_eq!(b.cursor, Cursor { row: 0, col: 1 });
        b.move_h();
        assert_eq!(b.cursor, Cursor { row: 0, col: 0 });
        b.move_h();
        assert_eq!(b.cursor, Cursor { row: 0, col: 0 }); // saturate
        b.move_j();
        assert_eq!(b.cursor.row, 1);
        b.move_k();
        assert_eq!(b.cursor.row, 0);
    }

    #[test]
    fn desired_col_preserved_across_short_line() {
        // col 2 on "abc", down to "de" (len 2 -> clamp to 1), back up restores 2.
        let mut b = EditBuffer::from_str("abc\nde\nfgh");
        b.cursor = Cursor { row: 0, col: 2 };
        b.move_j(); // onto "de": visually clamps
        assert_eq!(b.cursor.col, 1);
        b.move_j(); // onto "fgh": desired_col 2 restored
        assert_eq!(b.cursor.col, 2);
    }

    #[test]
    fn line_start_end_first_nonblank() {
        let mut b = EditBuffer::from_str("  ab");
        b.move_line_end();
        assert_eq!(b.cursor.col, 3);
        b.move_line_start();
        assert_eq!(b.cursor.col, 0);
        b.move_first_nonblank();
        assert_eq!(b.cursor.col, 2);
    }

    #[test]
    fn buffer_top_bottom() {
        let mut b = EditBuffer::from_str("a\nb\nc");
        b.move_buffer_bottom();
        assert_eq!(b.cursor.row, 2);
        b.move_buffer_top();
        assert_eq!(b.cursor.row, 0);
    }

    #[test]
    fn word_motions() {
        let mut b = EditBuffer::from_str("foo bar baz");
        b.move_word_forward();
        assert_eq!(b.cursor.col, 4); // start of "bar"
        b.move_word_end();
        assert_eq!(b.cursor.col, 6); // end of "bar"
        b.move_word_back();
        assert_eq!(b.cursor.col, 4); // back to "bar"
    }

    #[test]
    fn insert_char_and_newline_are_char_safe_cjk() {
        let mut b = EditBuffer::new();
        b.enter_insert_before();
        b.insert_char('中');
        b.insert_char('文');
        assert_eq!(b.lines, vec!["中文".to_string()]);
        assert_eq!(b.cursor.col, 2); // two chars
        b.insert_newline();
        assert_eq!(b.lines, vec!["中文".to_string(), String::new()]);
        assert_eq!(b.cursor, Cursor { row: 1, col: 0 });
    }

    #[test]
    fn backspace_joins_lines_at_col0() {
        let mut b = EditBuffer::from_str("ab\ncd");
        b.mode = Mode::Insert;
        b.cursor = Cursor { row: 1, col: 0 };
        b.backspace();
        assert_eq!(b.lines, vec!["abcd".to_string()]);
        assert_eq!(b.cursor, Cursor { row: 0, col: 2 });
    }

    #[test]
    fn a_appends_after_cursor_a_at_line_end() {
        let mut b = EditBuffer::from_str("ab");
        b.enter_insert_after(); // 'a' on col0 -> col1, insert mode
        assert_eq!(b.cursor.col, 1);
        assert!(matches!(b.mode, Mode::Insert));
        let mut b2 = EditBuffer::from_str("ab");
        b2.enter_insert_line_end(); // 'A' -> col2 (past last char), insert
        assert_eq!(b2.cursor.col, 2);
    }

    #[test]
    fn open_below_inserts_blank_line_and_enters_insert() {
        let mut b = EditBuffer::from_str("ab");
        b.open_below();
        assert_eq!(b.lines, vec!["ab".to_string(), String::new()]);
        assert_eq!(b.cursor, Cursor { row: 1, col: 0 });
        assert!(matches!(b.mode, Mode::Insert));
    }

    #[test]
    fn leave_insert_steps_back_when_past_end() {
        let mut b = EditBuffer::from_str("ab");
        b.mode = Mode::Insert;
        b.cursor = Cursor { row: 0, col: 2 }; // past last char
        b.leave_insert();
        assert!(matches!(b.mode, Mode::Normal));
        assert_eq!(b.cursor.col, 1); // vim behavior
    }
}
