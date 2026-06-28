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

#[cfg(test)]
mod tests {
    use super::*;

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
