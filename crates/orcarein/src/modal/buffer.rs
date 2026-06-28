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
}
