//! Vim normal-mode command parser: a small state machine that accumulates
//! `leading count → optional operator (+its own count) → motion / doubled
//! operator` into a `ParsedCommand` (spec: vim-modal-editor). Pure & testable:
//! the input is a normalized `KeyAction` (decoupled from crossterm) and the
//! output is data only — it never touches the `EditBuffer`. Task 9's reducer
//! dispatches the resulting `Motion`/`Op` onto the buffer.
//!
//! Assumption: the reducer handles non-command keys (`i`, `a`, `v`, `p`, `u`,
//! `x`, …) BEFORE delegating to this parser, so `feed` mostly sees
//! counts / operators / motions. Any key that doesn't fit the grammar is
//! swallowed (parser reset, `Parse::Pending`).

/// Normalized key, decoupled from crossterm so the parser/reducer are testable.
/// This is the COMPLETE key set used across the modal editor (consumed by the
/// Task 9 reducer and the Task 13 I/O loop, not just this parser).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Char(char),
    Esc,
    Enter,
    Backspace,
    CtrlC,
    CtrlD,
    CtrlR,
    Up,
    Down,
    Left,
    Right,
}

/// Cursor motions, 1:1 with `EditBuffer` motion methods (Task 3 / Task 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,          // h
    Right,         // l
    Up,            // k
    Down,          // j
    WordFwd,       // w
    WordBack,      // b
    WordEnd,       // e
    LineStart,     // 0
    LineEnd,       // $
    FirstNonBlank, // ^
    BufferTop,     // gg
    BufferBottom,  // G
}

/// Operators that combine with a motion (or are doubled for linewise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Delete,
    Change,
    Yank,
}

/// A fully recognized normal-mode command. `count` is always >= 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedCommand {
    /// Bare motion (no operator), e.g. `w`, `3j`.
    Motion { motion: Motion, count: usize },
    /// Operator + motion, e.g. `dw`, `2d3w` (count = 6).
    OpMotion {
        op: Op,
        motion: Motion,
        count: usize,
    },
    /// Doubled operator (linewise), e.g. `dd`, `yy`, `cc`, `Y`.
    OpLine { op: Op, count: usize },
    /// Esc — cancel any pending command.
    Cancel,
}

/// Result of feeding one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parse {
    /// More keys needed to complete a command.
    Pending,
    /// A complete command was recognized; parser state is reset.
    Done(ParsedCommand),
}

/// Accumulating normal-mode command parser.
#[derive(Debug, Default)]
pub struct CommandParser {
    count1: Option<usize>, // leading count, before the operator
    op: Option<Op>,
    count2: Option<usize>, // count after the operator
    pending_g: bool,       // saw a single `g`, waiting for the second
    /// History-navigation cursor (transient, reducer-owned). `None` means no
    /// recall in progress; reset to `None` on any non-history key so editing
    /// after a recall starts fresh.
    hist_idx: Option<usize>,
}

impl CommandParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        self.count1 = None;
        self.op = None;
        self.count2 = None;
        self.pending_g = false;
    }

    /// Combined count for the final command, each part defaulting to 1.
    fn total_count(&self) -> usize {
        self.count1.unwrap_or(1) * self.count2.unwrap_or(1)
    }

    /// Emit `Motion` or `OpMotion` for `motion`, then reset. `count` >= 1.
    fn finish_motion(&mut self, motion: Motion) -> Parse {
        let cmd = match self.op {
            Some(op) => ParsedCommand::OpMotion {
                op,
                motion,
                count: self.total_count(),
            },
            None => ParsedCommand::Motion {
                motion,
                count: self.count1.unwrap_or(1),
            },
        };
        self.reset();
        Parse::Done(cmd)
    }

    /// Map an operator char to `Op`, if it is one.
    fn op_for(c: char) -> Option<Op> {
        match c {
            'd' => Some(Op::Delete),
            'c' => Some(Op::Change),
            'y' => Some(Op::Yank),
            _ => None,
        }
    }

    /// Map a motion char to `Motion` (single-key motions; `gg` handled apart).
    fn motion_for(c: char) -> Option<Motion> {
        match c {
            'h' => Some(Motion::Left),
            'l' => Some(Motion::Right),
            'k' => Some(Motion::Up),
            'j' => Some(Motion::Down),
            'w' => Some(Motion::WordFwd),
            'b' => Some(Motion::WordBack),
            'e' => Some(Motion::WordEnd),
            '0' => Some(Motion::LineStart),
            '$' => Some(Motion::LineEnd),
            '^' => Some(Motion::FirstNonBlank),
            'G' => Some(Motion::BufferBottom),
            _ => None,
        }
    }

    pub fn feed(&mut self, key: KeyAction) -> Parse {
        // Esc always cancels, regardless of state.
        if key == KeyAction::Esc {
            self.reset();
            return Parse::Done(ParsedCommand::Cancel);
        }

        // Arrow keys behave exactly like h/j/k/l motions.
        let key = match key {
            KeyAction::Left => KeyAction::Char('h'),
            KeyAction::Right => KeyAction::Char('l'),
            KeyAction::Up => KeyAction::Char('k'),
            KeyAction::Down => KeyAction::Char('j'),
            other => other,
        };

        let KeyAction::Char(c) = key else {
            // Non-character keys (Enter, Backspace, Ctrl-*) are not part of the
            // normal-mode command grammar; the reducer handles them. Swallow.
            self.reset();
            return Parse::Pending;
        };

        // A single pending `g` expects another `g` for `gg` (BufferTop).
        if self.pending_g {
            self.pending_g = false;
            if c == 'g' {
                return self.finish_motion(Motion::BufferTop);
            }
            // Anything else after a lone `g` cancels the pending-g.
            self.reset();
            return Parse::Pending;
        }

        // Digit handling: leading `0` is the LineStart motion, NOT a count.
        // `0` only counts when a count is already being accumulated.
        if c.is_ascii_digit() {
            let d = (c as u8 - b'0') as usize;
            let accumulating_into_count2 = self.op.is_some();
            let slot = if accumulating_into_count2 {
                &mut self.count2
            } else {
                &mut self.count1
            };
            if c == '0' && slot.is_none() {
                // Leading 0 → LineStart motion (respecting any operator).
                return self.finish_motion(Motion::LineStart);
            }
            *slot = Some(slot.unwrap_or(0) * 10 + d);
            return Parse::Pending;
        }

        // Operator keys.
        if let Some(new_op) = Self::op_for(c) {
            match self.op {
                None => {
                    self.op = Some(new_op);
                    return Parse::Pending;
                }
                Some(existing) if existing == new_op => {
                    // Doubled operator (dd/cc/yy) → linewise.
                    let cmd = ParsedCommand::OpLine {
                        op: existing,
                        count: self.total_count(),
                    };
                    self.reset();
                    return Parse::Done(cmd);
                }
                Some(_) => {
                    // Mismatched second operator (e.g. `dy`): not valid; reset.
                    self.reset();
                    return Parse::Pending;
                }
            }
        }

        // Uppercase shorthands.
        match c {
            'Y' => {
                // Y == yy (linewise yank).
                let cmd = ParsedCommand::OpLine {
                    op: Op::Yank,
                    count: self.total_count(),
                };
                self.reset();
                return Parse::Done(cmd);
            }
            'D' => {
                // D == d$  (operator forced to Delete).
                self.op = Some(Op::Delete);
                return self.finish_motion(Motion::LineEnd);
            }
            'C' => {
                // C == c$  (operator forced to Change).
                self.op = Some(Op::Change);
                return self.finish_motion(Motion::LineEnd);
            }
            _ => {}
        }

        // Start of a `gg` sequence.
        if c == 'g' {
            self.pending_g = true;
            return Parse::Pending;
        }

        // Plain motions (incl. `0`/`$`/`^`/`G` handled in motion_for).
        if let Some(motion) = Self::motion_for(c) {
            return self.finish_motion(motion);
        }

        // Unrecognized key mid-parse: swallow and reset (reducer owns the rest).
        self.reset();
        Parse::Pending
    }
}

// ---- Reducer: drives the EditBuffer from keystrokes, emits side-effects. ----

use crate::modal::buffer::{Cursor, EditBuffer, Mode, Register};

/// A side-effect the reducer asks the I/O loop to perform. Pure data — the
/// reducer mutates `buf` directly for in-buffer edits and returns one of these
/// for actions the buffer cannot own (submit/cancel/EOF/clipboard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Submit,
    Cancel,
    Eof,
    Yank(Register),
}

/// Apply one normalized key to the buffer, dispatching by `buf.mode`. Pure
/// aside from mutating `buf`/`parser`; returns the side-effect to perform.
pub fn apply(
    buf: &mut EditBuffer,
    parser: &mut CommandParser,
    key: KeyAction,
    history: &[String],
) -> Effect {
    match buf.mode {
        Mode::Insert => apply_insert(buf, key),
        Mode::Normal => apply_normal(buf, parser, key, history),
        Mode::Visual(_) => apply_visual(buf, parser, key),
    }
}

fn apply_insert(buf: &mut EditBuffer, key: KeyAction) -> Effect {
    match key {
        KeyAction::Enter => buf.insert_newline(),
        KeyAction::Backspace => buf.backspace(),
        KeyAction::Char(c) => buf.insert_char(c),
        KeyAction::Esc => buf.leave_insert(),
        _ => {}
    }
    Effect::None
}

fn apply_normal(
    buf: &mut EditBuffer,
    parser: &mut CommandParser,
    key: KeyAction,
    history: &[String],
) -> Effect {
    // 1. Enter submits the line.
    if key == KeyAction::Enter {
        return Effect::Submit;
    }
    // 2. Ctrl-c cancels.
    if key == KeyAction::CtrlC {
        return Effect::Cancel;
    }
    // 3. Ctrl-d is EOF only on a single empty line; else ignored.
    if key == KeyAction::CtrlD {
        return if buf.text().is_empty() {
            Effect::Eof
        } else {
            Effect::None
        };
    }

    // 4. History recall. Entry: a single empty line. Continuation: a recall is
    // in progress (`hist_idx` set) AND the buffer still holds exactly that
    // recalled entry untouched — so k/j keep walking history, but a buffer the
    // user actually typed into is never clobbered.
    let empty_single = buf.lines.len() == 1 && buf.lines[0].is_empty();
    let recall_in_progress = parser
        .hist_idx
        .and_then(|i| history.get(i))
        .is_some_and(|entry| *entry == buf.text());
    if empty_single || recall_in_progress {
        let dir = match key {
            KeyAction::Char('k') | KeyAction::Up => Some(true), // older
            KeyAction::Char('j') | KeyAction::Down => Some(false), // newer
            _ => None,
        };
        if let Some(older) = dir {
            history_recall(buf, parser, history, older);
            return Effect::None;
        }
    }

    // 5. Reducer-owned single keys (these reset history navigation).
    if let KeyAction::Char(c) = key {
        let handled = match c {
            'i' => {
                buf.enter_insert_before();
                true
            }
            'a' => {
                buf.enter_insert_after();
                true
            }
            'I' => {
                buf.enter_insert_line_start();
                true
            }
            'A' => {
                buf.enter_insert_line_end();
                true
            }
            'o' => {
                buf.open_below();
                true
            }
            'O' => {
                buf.open_above();
                true
            }
            'v' => {
                buf.enter_visual_char();
                true
            }
            'V' => {
                buf.enter_visual_line();
                true
            }
            'x' => {
                buf.delete_char();
                true
            }
            'p' => {
                buf.paste_after();
                true
            }
            'P' => {
                buf.paste_before();
                true
            }
            'u' => {
                buf.undo();
                true
            }
            _ => false,
        };
        if handled {
            parser.hist_idx = None;
            return Effect::None;
        }
    }
    if key == KeyAction::CtrlR {
        buf.redo();
        parser.hist_idx = None;
        return Effect::None;
    }

    // 6. Everything else goes to the command parser.
    parser.hist_idx = None;
    match parser.feed(key) {
        Parse::Pending => Effect::None,
        Parse::Done(cmd) => exec_command(buf, cmd),
    }
}

/// Navigate `history` and load the selected entry into `buf`. `older` moves
/// toward older entries (k/Up), otherwise toward newer (j/Down).
fn history_recall(
    buf: &mut EditBuffer,
    parser: &mut CommandParser,
    history: &[String],
    older: bool,
) {
    if history.is_empty() {
        return;
    }
    let last = history.len() - 1;
    let new_idx = match parser.hist_idx {
        None => {
            // Fresh navigation: k starts at the newest (last) entry; j with no
            // history in progress also lands on the newest.
            last
        }
        Some(cur) => {
            if older {
                cur.saturating_sub(1)
            } else {
                (cur + 1).min(last)
            }
        }
    };
    parser.hist_idx = Some(new_idx);
    *buf = EditBuffer::from_str(&history[new_idx]);
    buf.mode = Mode::Normal;
    // Move cursor to the end (last row, clamped).
    buf.cursor.row = buf.lines.len().saturating_sub(1);
    buf.cursor.col = usize::MAX;
    buf.clamp_cursor();
}

/// Execute a parsed normal-mode command against the buffer.
fn exec_command(buf: &mut EditBuffer, cmd: ParsedCommand) -> Effect {
    match cmd {
        ParsedCommand::Motion { motion, count } => {
            for _ in 0..count.max(1) {
                apply_motion(buf, motion);
            }
            Effect::None
        }
        ParsedCommand::OpLine { op, count } => exec_op_line(buf, op, count.max(1)),
        ParsedCommand::OpMotion { op, motion, count } => {
            exec_op_motion(buf, op, motion, count.max(1))
        }
        ParsedCommand::Cancel => Effect::None,
    }
}

/// Apply a single motion to the cursor.
fn apply_motion(buf: &mut EditBuffer, motion: Motion) {
    match motion {
        Motion::Left => buf.move_h(),
        Motion::Right => buf.move_l(),
        Motion::Up => buf.move_k(),
        Motion::Down => buf.move_j(),
        Motion::WordFwd => buf.move_word_forward(),
        Motion::WordBack => buf.move_word_back(),
        Motion::WordEnd => buf.move_word_end(),
        Motion::LineStart => buf.move_line_start(),
        Motion::LineEnd => buf.move_line_end(),
        Motion::FirstNonBlank => buf.move_first_nonblank(),
        Motion::BufferTop => buf.move_buffer_top(),
        Motion::BufferBottom => buf.move_buffer_bottom(),
    }
}

/// Linewise operator on `count` lines starting at the cursor row.
fn exec_op_line(buf: &mut EditBuffer, op: Op, count: usize) -> Effect {
    match op {
        Op::Delete => {
            for _ in 0..count {
                buf.delete_line();
            }
            Effect::None
        }
        Op::Change => {
            // MVP: change the first line only when count > 1 (acceptable).
            buf.change_line();
            Effect::None
        }
        Op::Yank => {
            if count <= 1 {
                buf.yank_line();
            } else {
                // Build a linewise register from `count` rows from the cursor.
                let start = buf.cursor.row;
                let end = (start + count).min(buf.lines.len());
                let text = buf.lines[start..end].join("\n");
                buf.register = Register {
                    text,
                    linewise: true,
                };
            }
            Effect::Yank(buf.register.clone())
        }
    }
}

/// Operator + motion: form an inclusive char range [start, end] by applying the
/// motion `count` times from the cursor, then act on it. MVP latitude: exact
/// vim inclusive/exclusive parity is NOT required — this is a reasonable
/// inclusive range.
fn exec_op_motion(buf: &mut EditBuffer, op: Op, motion: Motion, count: usize) -> Effect {
    let start = buf.cursor.clone();
    for _ in 0..count {
        apply_motion(buf, motion);
    }
    let end = buf.cursor.clone();
    // Restore the cursor before operating (delete_range/yank_range reposition).
    buf.cursor = start.clone();

    // If the motion didn't move (e.g. already at boundary), there is nothing to
    // operate on with a clean inclusive range; act on the single char.
    let (lo, hi) = order(start, end);
    match op {
        Op::Delete => {
            buf.delete_range(lo, hi);
            Effect::None
        }
        Op::Yank => {
            buf.yank_range(lo, hi);
            Effect::Yank(buf.register.clone())
        }
        Op::Change => {
            buf.delete_range(lo, hi);
            buf.enter_insert_before();
            Effect::None
        }
    }
}

/// Order two cursors in document order (lo <= hi).
fn order(a: Cursor, b: Cursor) -> (Cursor, Cursor) {
    if (a.row, a.col) <= (b.row, b.col) {
        (a, b)
    } else {
        (b, a)
    }
}

fn apply_visual(buf: &mut EditBuffer, parser: &mut CommandParser, key: KeyAction) -> Effect {
    match key {
        KeyAction::Esc => {
            buf.mode = Mode::Normal;
            buf.anchor = None;
            Effect::None
        }
        KeyAction::Char('d') | KeyAction::Char('x') => {
            buf.delete_selection();
            Effect::None
        }
        KeyAction::Char('y') => {
            buf.yank_selection();
            Effect::Yank(buf.register.clone())
        }
        KeyAction::Char('c') => {
            buf.change_selection();
            Effect::None
        }
        // Motion keys extend the selection (anchor stays put). Feed through the
        // parser; only act on a recognized Motion (operators are ignored here).
        _ => {
            match parser.feed(key) {
                Parse::Done(ParsedCommand::Motion { motion, count }) => {
                    for _ in 0..count.max(1) {
                        apply_motion(buf, motion);
                    }
                }
                Parse::Done(ParsedCommand::Cancel) => {
                    // Esc arrives here only via parser for arrow-less Esc; but we
                    // handle Esc above, so this is unreachable in practice.
                    buf.mode = Mode::Normal;
                    buf.anchor = None;
                }
                _ => {}
            }
            Effect::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modal::buffer::{EditBuffer, Mode, Register};

    #[test]
    fn insert_enter_inserts_newline_normal_enter_submits() {
        let mut b = EditBuffer::from_str("hi");
        let mut p = CommandParser::new();
        b.mode = Mode::Insert;
        assert_eq!(apply(&mut b, &mut p, KeyAction::Enter, &[]), Effect::None);
        assert_eq!(b.lines.len(), 2); // newline inserted
        b.mode = Mode::Normal;
        assert_eq!(apply(&mut b, &mut p, KeyAction::Enter, &[]), Effect::Submit);
    }

    #[test]
    fn ctrl_c_cancels_ctrl_d_eof_only_when_empty() {
        let mut p = CommandParser::new();
        let mut empty = EditBuffer::new();
        assert_eq!(
            apply(&mut empty, &mut p, KeyAction::CtrlD, &[]),
            Effect::Eof
        );
        let mut nonempty = EditBuffer::from_str("x");
        assert_eq!(
            apply(&mut nonempty, &mut p, KeyAction::CtrlD, &[]),
            Effect::None
        ); // ignored
        assert_eq!(
            apply(&mut nonempty, &mut p, KeyAction::CtrlC, &[]),
            Effect::Cancel
        );
    }

    #[test]
    fn yank_emits_yank_effect_for_clipboard() {
        let mut b = EditBuffer::from_str("abc");
        let mut p = CommandParser::new();
        let e = apply(&mut b, &mut p, KeyAction::Char('y'), &[]); // expect 'y' then 'y' -> yy
        assert_eq!(e, Effect::None); // first y pending
        let e2 = apply(&mut b, &mut p, KeyAction::Char('y'), &[]);
        assert_eq!(
            e2,
            Effect::Yank(Register {
                text: "abc".into(),
                linewise: true
            })
        );
    }

    #[test]
    fn history_recall_only_on_empty_single_line() {
        let hist = vec!["prev1".to_string(), "prev2".to_string()];
        let mut p = CommandParser::new();
        // Empty buffer, Normal: 'k' recalls last history entry.
        let mut empty = EditBuffer::new();
        apply(&mut empty, &mut p, KeyAction::Char('k'), &hist);
        assert_eq!(empty.text(), "prev2");
        // Non-empty buffer: 'k' is a cursor motion, NOT history.
        let mut nonempty = EditBuffer::from_str("typed");
        let before = nonempty.text();
        apply(&mut nonempty, &mut p, KeyAction::Char('k'), &hist);
        assert_eq!(nonempty.text(), before); // unchanged — no history clobber
    }

    #[test]
    fn dw_deletes_word_forward() {
        // "foo bar" with dw from col 0 deletes through start of "bar" inclusive
        // range [0, motion-target]. MVP-reasonable inclusive range.
        let mut b = EditBuffer::from_str("foo bar");
        let mut p = CommandParser::new();
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('d'), &[]),
            Effect::None
        );
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('w'), &[]),
            Effect::None
        );
        // word_forward lands on 'b' (col 4); inclusive delete [0,4] -> "ar".
        assert_eq!(b.text(), "ar");
    }

    #[test]
    fn d_dollar_deletes_to_line_end() {
        let mut b = EditBuffer::from_str("abcd");
        b.cursor.col = 1;
        let mut p = CommandParser::new();
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('d'), &[]),
            Effect::None
        );
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('$'), &[]),
            Effect::None
        );
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn count_motion_moves_multiple() {
        let mut b = EditBuffer::from_str("abcdef");
        let mut p = CommandParser::new();
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('2'), &[]),
            Effect::None
        );
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('l'), &[]),
            Effect::None
        );
        assert_eq!(b.cursor.col, 2);
    }

    #[test]
    fn visual_delete_via_apply() {
        let mut b = EditBuffer::from_str("abcdef");
        let mut p = CommandParser::new();
        // enter visual char, extend selection, delete.
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('v'), &[]),
            Effect::None
        );
        assert!(matches!(b.mode, Mode::Visual(_)));
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('l'), &[]),
            Effect::None
        );
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('l'), &[]),
            Effect::None
        );
        // selection abc inclusive
        assert_eq!(
            apply(&mut b, &mut p, KeyAction::Char('d'), &[]),
            Effect::None
        );
        assert_eq!(b.text(), "def");
        assert!(matches!(b.mode, Mode::Normal));
    }

    #[test]
    fn history_j_after_k_moves_newer() {
        let hist = vec!["prev1".to_string(), "prev2".to_string()];
        let mut p = CommandParser::new();
        let mut b = EditBuffer::new();
        // k -> prev2 (newest), k -> prev1 (older), j -> prev2 again
        apply(&mut b, &mut p, KeyAction::Char('k'), &hist);
        assert_eq!(b.text(), "prev2");
        apply(&mut b, &mut p, KeyAction::Char('k'), &hist);
        assert_eq!(b.text(), "prev1");
        apply(&mut b, &mut p, KeyAction::Char('j'), &hist);
        assert_eq!(b.text(), "prev2");
    }

    #[test]
    fn plain_motion_no_count() {
        let mut p = CommandParser::new();
        assert_eq!(
            p.feed(KeyAction::Char('w')),
            Parse::Done(ParsedCommand::Motion {
                motion: Motion::WordFwd,
                count: 1
            })
        );
    }

    #[test]
    fn count_prefix_multiplies_motion() {
        let mut p = CommandParser::new();
        assert_eq!(p.feed(KeyAction::Char('3')), Parse::Pending);
        assert_eq!(
            p.feed(KeyAction::Char('j')),
            Parse::Done(ParsedCommand::Motion {
                motion: Motion::Down,
                count: 3
            })
        );
    }

    #[test]
    fn operator_motion_with_double_count() {
        let mut p = CommandParser::new();
        for k in ['2', 'd', '3', 'w'] {
            let r = p.feed(KeyAction::Char(k));
            if k == 'w' {
                assert_eq!(
                    r,
                    Parse::Done(ParsedCommand::OpMotion {
                        op: Op::Delete,
                        motion: Motion::WordFwd,
                        count: 6
                    })
                );
            } else {
                assert_eq!(r, Parse::Pending);
            }
        }
    }

    #[test]
    fn doubled_operator_is_linewise() {
        let mut p = CommandParser::new();
        assert_eq!(p.feed(KeyAction::Char('d')), Parse::Pending);
        assert_eq!(
            p.feed(KeyAction::Char('d')),
            Parse::Done(ParsedCommand::OpLine {
                op: Op::Delete,
                count: 1
            })
        );
    }

    #[test]
    fn esc_resets_pending() {
        let mut p = CommandParser::new();
        let _ = p.feed(KeyAction::Char('2'));
        assert_eq!(p.feed(KeyAction::Esc), Parse::Done(ParsedCommand::Cancel));
        // parser cleared
        assert_eq!(
            p.feed(KeyAction::Char('j')),
            Parse::Done(ParsedCommand::Motion {
                motion: Motion::Down,
                count: 1
            })
        );
    }
}
