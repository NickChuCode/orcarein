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

    // ---- Undo bookkeeping. undo()/redo() consume these stacks (see below). ----

    /// Snapshot the current state before a mutation and invalidate redo.
    fn push_undo(&mut self) {
        self.undo.push(Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor.clone(),
        });
        self.redo.clear();
    }

    /// `u` — undo the last mutation. No-op on an empty undo stack. Pushes the
    /// current state onto the redo stack, then restores the popped snapshot.
    /// Manages the stacks directly (does NOT call `push_undo`).
    pub fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(Snapshot {
                lines: self.lines.clone(),
                cursor: self.cursor.clone(),
            });
            self.lines = prev.lines;
            self.cursor = prev.cursor;
            self.clamp_cursor();
        }
    }

    /// `Ctrl-r` — redo the last undone mutation. Symmetric to `undo`. No-op on
    /// an empty redo stack. `push_undo` (called by mutators) clears redo, so a
    /// fresh edit invalidates the redo history.
    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(Snapshot {
                lines: self.lines.clone(),
                cursor: self.cursor.clone(),
            });
            self.lines = next.lines;
            self.cursor = next.cursor;
            self.clamp_cursor();
        }
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

    // ---- Normal-mode operators: delete / change / yank + paste. ----

    /// `x` — delete the char under the cursor (charwise register). No-op on an
    /// empty line.
    pub fn delete_char(&mut self) {
        let row = self.cursor.row;
        let col = self.cursor.col;
        if col >= self.row_chars(row) {
            return; // empty line or past end: nothing under cursor
        }
        self.push_undo();
        let line = &mut self.lines[row];
        let start = Self::byte_at(line, col);
        let removed = line[start..].chars().next().unwrap();
        line.remove(start);
        self.register = Register {
            text: removed.to_string(),
            linewise: false,
        };
        self.clamp_cursor();
    }

    // Note: `D` (delete-to-line-end) and `C` (change-to-line-end) are handled by
    // the reducer via `OpMotion { motion: LineEnd }` → `delete_range`, which is
    // equivalent (delete cursor..=last-char) and already covered by the `d$`
    // reducer test — so no dedicated buffer methods are needed for them.

    /// `dd` — delete the whole current line (linewise register). Leaves one
    /// empty line if it was the last remaining line.
    pub fn delete_line(&mut self) {
        self.push_undo();
        let row = self.cursor.row;
        let removed = self.lines.remove(row);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.register = Register {
            text: removed,
            linewise: true,
        };
        // Stay on the same row index (now the following line), clamped.
        self.cursor.col = 0;
        self.clamp_cursor();
    }

    /// `cc` — change the whole line: clear it (keep the line), register the old
    /// content linewise, enter Insert at col 0.
    pub fn change_line(&mut self) {
        self.push_undo();
        let row = self.cursor.row;
        let old = std::mem::take(&mut self.lines[row]);
        self.register = Register {
            text: old,
            linewise: true,
        };
        self.cursor.col = 0;
        self.mode = Mode::Insert;
        self.clamp_cursor();
    }

    /// `yy` / `Y` — yank the current line into the register (linewise). The
    /// buffer and cursor are unchanged (no undo push).
    pub fn yank_line(&mut self) {
        let row = self.cursor.row;
        let text = self.lines.get(row).cloned().unwrap_or_default();
        self.register = Register {
            text,
            linewise: true,
        };
    }

    /// Normalize a pair of cursors into (start, end) document order.
    fn ordered(a: Cursor, b: Cursor) -> (Cursor, Cursor) {
        if (a.row, a.col) <= (b.row, b.col) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// Collect the inclusive charwise text between `start` and `end` (across
    /// lines). Used by both `yank_range` and `delete_range`.
    fn collect_range(&self, start: &Cursor, end: &Cursor) -> String {
        if start.row == end.row {
            let line = match self.lines.get(start.row) {
                Some(l) => l,
                None => return String::new(),
            };
            let from = Self::byte_at(line, start.col);
            let to = Self::byte_at(line, end.col + 1);
            return line[from..to.min(line.len())].to_string();
        }
        let mut out = String::new();
        // First line: from start.col to its end.
        if let Some(first) = self.lines.get(start.row) {
            let from = Self::byte_at(first, start.col);
            out.push_str(&first[from..]);
        }
        out.push('\n');
        // Whole middle lines.
        for r in (start.row + 1)..end.row {
            if let Some(mid) = self.lines.get(r) {
                out.push_str(mid);
            }
            out.push('\n');
        }
        // Last line: from its start through end.col (inclusive).
        if let Some(last) = self.lines.get(end.row) {
            let to = Self::byte_at(last, end.col + 1);
            out.push_str(&last[..to.min(last.len())]);
        }
        out
    }

    /// Charwise inclusive yank of the range `[start, end]` (no mutation, no
    /// undo). Cursors may be given in any order.
    pub fn yank_range(&mut self, start: Cursor, end: Cursor) {
        let (start, end) = Self::ordered(start, end);
        let text = self.collect_range(&start, &end);
        self.register = Register {
            text,
            linewise: false,
        };
    }

    /// Charwise inclusive delete of the range `[start, end]` (charwise
    /// register). Cursor lands on the start of the deleted range.
    pub fn delete_range(&mut self, start: Cursor, end: Cursor) {
        self.push_undo();
        self.delete_range_no_undo(start, end);
    }

    /// Charwise inclusive delete without an undo push — the shared core of
    /// `delete_range` and the Visual charwise delete (which pushes undo once
    /// itself to guarantee a single snapshot per operation).
    fn delete_range_no_undo(&mut self, start: Cursor, end: Cursor) {
        let (start, end) = Self::ordered(start, end);
        let text = self.collect_range(&start, &end);
        if start.row == end.row {
            let line = &mut self.lines[start.row];
            let from = Self::byte_at(line, start.col);
            let to = Self::byte_at(line, end.col + 1).min(line.len());
            line.replace_range(from..to, "");
        } else {
            // Head of the first line, tail after end.col on the last line.
            let head: String = {
                let first = &self.lines[start.row];
                first[..Self::byte_at(first, start.col)].to_string()
            };
            let tail: String = {
                let last = &self.lines[end.row];
                let after = Self::byte_at(last, end.col + 1).min(last.len());
                last[after..].to_string()
            };
            // Remove the inner lines (end.row down to start.row+1), then splice.
            self.lines.drain((start.row + 1)..=end.row);
            self.lines[start.row] = head + &tail;
        }
        self.register = Register {
            text,
            linewise: false,
        };
        self.cursor = start;
        self.clamp_cursor();
    }

    /// Insert charwise `text` (which MAY contain '\n' from a cross-line yank) at
    /// char position (row, insert_col), preserving the "no line holds a '\n'"
    /// invariant by splitting the line when needed. Returns the cursor position
    /// of the last inserted char (for p/P landing). Char-boundary safe.
    fn splice_charwise(&mut self, row: usize, insert_col: usize, text: &str) -> Cursor {
        if !text.contains('\n') {
            let pasted = text.chars().count();
            let line = &mut self.lines[row];
            let at = Self::byte_at(line, insert_col);
            line.insert_str(at, text);
            return Cursor {
                row,
                col: insert_col + pasted.saturating_sub(1),
            };
        }
        // Multi-line: split the current line at insert_col and weave segments in.
        let segs: Vec<&str> = text.split('\n').collect();
        let n = segs.len();
        let line = self.lines[row].clone();
        let at = Self::byte_at(&line, insert_col);
        let (before, after) = (&line[..at], &line[at..]);
        self.lines[row] = format!("{before}{}", segs[0]);
        for (i, seg) in segs[1..].iter().enumerate() {
            let idx = row + 1 + i;
            if i + 1 == n - 1 {
                // Last segment gets the original line's tail appended.
                self.lines.insert(idx, format!("{seg}{after}"));
            } else {
                self.lines.insert(idx, (*seg).to_string());
            }
        }
        Cursor {
            row: row + n - 1,
            col: segs[n - 1].chars().count().saturating_sub(1),
        }
    }

    /// `p` — paste after the cursor. Linewise → new line(s) below the cursor
    /// row; charwise → inline after the cursor (splitting on any '\n').
    pub fn paste_after(&mut self) {
        if self.register.text.is_empty() && !self.register.linewise {
            return;
        }
        self.push_undo();
        if self.register.linewise {
            let at = self.cursor.row + 1;
            let new_lines: Vec<String> =
                self.register.text.split('\n').map(str::to_string).collect();
            for (i, l) in new_lines.into_iter().enumerate() {
                self.lines.insert(at + i, l);
            }
            self.cursor = Cursor { row: at, col: 0 };
        } else {
            let row = self.cursor.row;
            // Paste after the cursor char: at col+1 unless the line is empty.
            let insert_col = if self.row_chars(row) == 0 {
                0
            } else {
                self.cursor.col + 1
            };
            let text = self.register.text.clone();
            self.cursor = self.splice_charwise(row, insert_col, &text);
        }
        self.clamp_cursor();
    }

    // ---- Visual mode (v / V) + selection operators. ----

    /// `v` — enter charwise Visual mode, anchoring at the current cursor.
    pub fn enter_visual_char(&mut self) {
        self.anchor = Some(self.cursor.clone());
        self.mode = Mode::Visual(VisualKind::Char);
    }

    /// `V` — enter linewise Visual mode, anchoring at the current cursor.
    pub fn enter_visual_line(&mut self) {
        self.anchor = Some(self.cursor.clone());
        self.mode = Mode::Visual(VisualKind::Line);
    }

    /// The selection extent as (lo, hi) in document order (lo <= hi). Pure
    /// (read-only). With no anchor, both ends are the current cursor.
    pub fn selection_range(&self) -> (Cursor, Cursor) {
        match &self.anchor {
            Some(a) => Self::ordered(a.clone(), self.cursor.clone()),
            None => (self.cursor.clone(), self.cursor.clone()),
        }
    }

    /// `y` in Visual mode. Charwise → inclusive charwise yank of the selection;
    /// linewise → the selected whole rows joined by '\n' (no trailing newline),
    /// linewise register. Buffer unchanged; returns to Normal and clears anchor.
    pub fn yank_selection(&mut self) {
        let (lo, hi) = self.selection_range();
        match self.mode {
            Mode::Visual(VisualKind::Line) => {
                let text = self.lines[lo.row..=hi.row.min(self.lines.len() - 1)].join("\n");
                self.register = Register {
                    text,
                    linewise: true,
                };
            }
            _ => {
                // Charwise inclusive (also the fallback if not in Visual).
                self.yank_range(lo, hi);
            }
        }
        self.mode = Mode::Normal;
        self.anchor = None;
        self.clamp_cursor();
    }

    /// `d`/`x` in Visual mode. Charwise → inclusive charwise delete; linewise →
    /// remove the selected whole rows (linewise register, keep >=1 line). Pushes
    /// one undo snapshot, returns to Normal and clears anchor.
    pub fn delete_selection(&mut self) {
        self.push_undo();
        self.delete_selection_inner();
        self.mode = Mode::Normal;
        self.anchor = None;
        self.clamp_cursor();
    }

    /// `c`/`s` in Visual mode: delete the selection, then enter Insert mode.
    /// Exactly one undo snapshot is pushed.
    pub fn change_selection(&mut self) {
        self.push_undo();
        self.delete_selection_inner();
        self.anchor = None;
        self.mode = Mode::Insert;
        self.clamp_cursor();
    }

    /// Shared delete logic for the selection. Does NOT push undo, set mode, or
    /// clear the anchor — callers own that to guarantee a single snapshot.
    fn delete_selection_inner(&mut self) {
        let (lo, hi) = self.selection_range();
        match self.mode {
            Mode::Visual(VisualKind::Line) => {
                let last = hi.row.min(self.lines.len() - 1);
                let text = self.lines[lo.row..=last].join("\n");
                self.lines.drain(lo.row..=last);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.register = Register {
                    text,
                    linewise: true,
                };
                self.cursor = Cursor {
                    row: lo.row,
                    col: 0,
                };
            }
            _ => {
                // Charwise inclusive delete via the shared no-undo core (the
                // caller already pushed exactly one snapshot).
                self.delete_range_no_undo(lo, hi);
            }
        }
    }

    /// `P` — paste before the cursor. Linewise → new line(s) above the cursor
    /// row; charwise → inline before the cursor.
    pub fn paste_before(&mut self) {
        if self.register.text.is_empty() && !self.register.linewise {
            return;
        }
        self.push_undo();
        if self.register.linewise {
            let at = self.cursor.row;
            let new_lines: Vec<String> =
                self.register.text.split('\n').map(str::to_string).collect();
            for (i, l) in new_lines.into_iter().enumerate() {
                self.lines.insert(at + i, l);
            }
            self.cursor = Cursor { row: at, col: 0 };
        } else {
            let row = self.cursor.row;
            let insert_col = self.cursor.col;
            let text = self.register.text.clone();
            self.cursor = self.splice_charwise(row, insert_col, &text);
        }
        self.clamp_cursor();
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

    #[test]
    fn x_deletes_char_under_cursor_charwise_register() {
        let mut b = EditBuffer::from_str("abc");
        b.cursor.col = 1;
        b.delete_char();
        assert_eq!(b.lines, vec!["ac".to_string()]);
        assert_eq!(
            b.register,
            Register {
                text: "b".into(),
                linewise: false
            }
        );
    }

    #[test]
    fn dd_deletes_line_linewise_register() {
        let mut b = EditBuffer::from_str("a\nb\nc");
        b.cursor.row = 1;
        b.delete_line();
        assert_eq!(b.lines, vec!["a".to_string(), "c".to_string()]);
        assert!(b.register.linewise);
        assert_eq!(b.register.text, "b");
    }

    #[test]
    fn yy_then_p_opens_new_line_below() {
        let mut b = EditBuffer::from_str("a\nb");
        b.yank_line(); // register = "a" linewise
        b.paste_after(); // p: linewise -> new line below cursor row
        assert_eq!(
            b.lines,
            vec!["a".to_string(), "a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn charwise_p_pastes_inline_after_cursor() {
        let mut b = EditBuffer::from_str("ac");
        b.register = Register {
            text: "b".into(),
            linewise: false,
        };
        // cursor col0 on 'a'; p pastes after -> "abc", cursor on pasted 'b'
        b.paste_after();
        assert_eq!(b.lines, vec!["abc".to_string()]);
        assert_eq!(b.cursor.col, 1);
    }

    #[test]
    fn charwise_p_with_multiline_register_splits_into_lines() {
        // A charwise register can hold '\n' (cross-line v-yank). Pasting it must
        // NOT embed a newline into one line — it splits the line instead.
        let mut b = EditBuffer::from_str("XY");
        b.register = Register {
            text: "a\nb".into(),
            linewise: false,
        };
        b.cursor.col = 0; // on 'X'; p pastes after X
        b.paste_after();
        assert_eq!(b.lines, vec!["Xa".to_string(), "bY".to_string()]);
        assert!(
            b.lines.iter().all(|l| !l.contains('\n')),
            "invariant: no line holds a newline"
        );
        // cursor on last pasted char 'b' -> row1 col0
        assert_eq!(b.cursor, Cursor { row: 1, col: 0 });
    }

    #[test]
    fn charwise_cap_p_with_multiline_register_splits_into_lines() {
        let mut b = EditBuffer::from_str("XY");
        b.register = Register {
            text: "a\nb".into(),
            linewise: false,
        };
        b.cursor.col = 1; // on 'Y'; P pastes before Y
        b.paste_before();
        assert_eq!(b.lines, vec!["Xa".to_string(), "bY".to_string()]);
        assert!(b.lines.iter().all(|l| !l.contains('\n')));
    }

    #[test]
    fn delete_range_cross_line_charwise() {
        // "abc\ndef\nghi": delete inclusive 'b'..'h' (0,1)..(2,1).
        // Keeps head "a" + tail after 'h' = "i" -> "ai".
        let mut b = EditBuffer::from_str("abc\ndef\nghi");
        b.delete_range(Cursor { row: 0, col: 1 }, Cursor { row: 2, col: 1 });
        assert_eq!(b.lines, vec!["ai".to_string()]);
        assert_eq!(b.register.text, "bc\ndef\ngh");
        assert!(!b.register.linewise);
        assert_eq!(b.cursor, Cursor { row: 0, col: 1 });
    }

    #[test]
    fn yank_range_cross_line_does_not_mutate() {
        let mut b = EditBuffer::from_str("abc\ndef");
        b.yank_range(Cursor { row: 0, col: 1 }, Cursor { row: 1, col: 0 });
        assert_eq!(b.lines, vec!["abc".to_string(), "def".to_string()]);
        assert_eq!(b.register.text, "bc\nd");
        assert!(!b.register.linewise);
    }

    #[test]
    fn visual_char_selection_range_normalizes() {
        let mut b = EditBuffer::from_str("abcdef");
        b.cursor.col = 4;
        b.enter_visual_char(); // anchor at col4
        b.cursor.col = 1; // move left
        let (lo, hi) = b.selection_range();
        assert_eq!((lo.col, hi.col), (1, 4));
    }

    #[test]
    fn visual_char_yank_is_charwise() {
        let mut b = EditBuffer::from_str("abcdef");
        b.enter_visual_char(); // anchor col0
        b.cursor.col = 2; // select a,b,c (inclusive)
        b.yank_selection();
        assert_eq!(
            b.register,
            Register {
                text: "abc".into(),
                linewise: false
            }
        );
        assert!(matches!(b.mode, Mode::Normal));
        assert!(b.anchor.is_none());
    }

    #[test]
    fn visual_line_yank_is_linewise() {
        let mut b = EditBuffer::from_str("a\nb\nc");
        b.enter_visual_line(); // anchor row0
        b.cursor.row = 1; // select rows 0..=1
        b.yank_selection();
        assert!(b.register.linewise);
        assert_eq!(b.register.text, "a\nb");
    }

    #[test]
    fn visual_delete_removes_selection() {
        let mut b = EditBuffer::from_str("abcdef");
        b.enter_visual_char();
        b.cursor.col = 2; // select abc
        b.delete_selection();
        assert_eq!(b.lines, vec!["def".to_string()]);
        assert!(matches!(b.mode, Mode::Normal));
    }

    #[test]
    fn undo_restores_prior_state_redo_reapplies() {
        let mut b = EditBuffer::from_str("ab");
        b.mode = Mode::Insert;
        b.cursor.col = 2;
        b.insert_char('c'); // "abc"
        assert_eq!(b.lines, vec!["abc".to_string()]);
        b.undo();
        assert_eq!(b.lines, vec!["ab".to_string()]);
        b.redo();
        assert_eq!(b.lines, vec!["abc".to_string()]);
    }

    #[test]
    fn new_change_clears_redo() {
        let mut b = EditBuffer::from_str("a");
        b.mode = Mode::Insert;
        b.cursor.col = 1;
        b.insert_char('b'); // "ab"
        b.undo(); // "a"
        b.insert_char('c'); // "ac" — clears redo
        b.redo(); // no-op
        assert_eq!(b.lines, vec!["ac".to_string()]);
    }

    #[test]
    fn undo_on_empty_stack_is_noop() {
        let mut b = EditBuffer::from_str("a");
        b.undo();
        assert_eq!(b.lines, vec!["a".to_string()]);
    }
}
