//! Pure render of an [`EditBuffer`] into a [`RenderView`] (spec §6): the visible
//! line window (scrolled so the cursor row stays in view), visual-selection
//! highlight spans, the cursor's screen coordinate (display-width aware, CJK=2),
//! and a status line. No terminal I/O — fully unit-tested. The raw-mode I/O loop
//! (a later task) consumes this view to paint the inline viewport.

use crate::header::disp_width;
use crate::modal::buffer::{Cursor, EditBuffer, Mode, VisualKind};

/// One visible body line, split into `(segment, highlighted)` spans. A plain
/// line is a single `(text, false)` span; a visual selection introduces a
/// highlighted segment (and the un-highlighted remainder around it).
pub struct RenderLine {
    pub spans: Vec<(String, bool)>,
}

/// The pure result of rendering: visible lines, the cursor's screen position
/// within the inline viewport (text-relative; the I/O layer offsets it by the
/// gutter), and the structured status fields the I/O layer styles (mode badge,
/// position, key hint). `mode` drives the gutter / badge / selection color.
pub struct RenderView {
    pub lines: Vec<RenderLine>,
    pub cursor_screen: (u16, u16), // (row, col) within the inline viewport
    pub mode: Mode,
    pub badge: &'static str, // mode word for the status badge
    pub pos: String,         // "row,col" / "row,c1-c2" / "r1-r2"
    pub hint: String,        // per-mode key hint
}

/// Split `line` (the row at `row`) into highlighted/plain segments per the
/// selection `sel = (lo, hi)` (inclusive, document order) and visual `kind`.
/// With `sel == None` the whole line is one plain span.
///
/// Char-visual highlights the char range on each selected row, inclusive of
/// `hi.col` on `hi.row`; intermediate rows highlight to end-of-line, and the
/// first/last rows clip at `lo.col` / `hi.col`. Line-visual highlights the WHOLE
/// line for any row in `[lo.row, hi.row]`. All slicing is char-boundary safe
/// (via `char_indices`), so a CJK char is never split.
pub fn selection_segments(
    line: &str,
    row: usize,
    sel: Option<(Cursor, Cursor)>,
    kind: VisualKind,
) -> Vec<(String, bool)> {
    let (lo, hi) = match sel {
        Some(pair) => pair,
        None => return vec![(line.to_string(), false)],
    };
    // Row entirely outside the selection extent → plain.
    if row < lo.row || row > hi.row {
        return vec![(line.to_string(), false)];
    }

    let n = line.chars().count();
    match kind {
        VisualKind::Line => {
            // Whole-line highlight for any row in the selected span. An empty
            // line yields no span (nothing to paint).
            if line.is_empty() {
                Vec::new()
            } else {
                vec![(line.to_string(), true)]
            }
        }
        VisualKind::Char => {
            // Char range [start, end] (inclusive char indices) for this row.
            let start = if row == lo.row { lo.col } else { 0 };
            // On the last row clip at hi.col (inclusive); otherwise to EOL.
            let end_incl = if row == hi.row {
                hi.col
            } else {
                n.saturating_sub(1)
            };
            char_span_split(line, start, end_incl)
        }
    }
}

/// Split `line` into `[before, highlighted, after]` plain/highlight spans where
/// the highlight covers the inclusive char range `[start, end_incl]`. Empty
/// segments are omitted. Char-boundary safe.
fn char_span_split(line: &str, start: usize, end_incl: usize) -> Vec<(String, bool)> {
    let n = line.chars().count();
    if n == 0 {
        // Nothing to highlight on an empty line.
        return Vec::new();
    }
    let start = start.min(n);
    // Convert inclusive end to an exclusive bound, clamped to the line length.
    let end_excl = end_incl.saturating_add(1).min(n);
    if start >= end_excl {
        // Degenerate range → plain line.
        return vec![(line.to_string(), false)];
    }
    let b_start = byte_at(line, start);
    let b_end = byte_at(line, end_excl);
    let mut out = Vec::new();
    if b_start > 0 {
        out.push((line[..b_start].to_string(), false));
    }
    out.push((line[b_start..b_end].to_string(), true));
    if b_end < line.len() {
        out.push((line[b_end..].to_string(), false));
    }
    out
}

/// Byte offset of char index `col` within `line` (end-of-line if past it).
/// Mirrors `EditBuffer::byte_at` (which is private) — boundary safe.
fn byte_at(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
}

/// Human label for the status badge.
fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "NORMAL",
        Mode::Insert => "INSERT",
        Mode::Visual(VisualKind::Char) => "VISUAL",
        Mode::Visual(VisualKind::Line) => "V-LINE",
    }
}

/// Cursor / selection position text for the status line.
fn position_text(buf: &EditBuffer) -> String {
    match buf.mode {
        Mode::Visual(VisualKind::Line) => {
            let (lo, hi) = buf.selection_range();
            format!("{}-{}", lo.row + 1, hi.row + 1)
        }
        Mode::Visual(VisualKind::Char) => {
            let (lo, hi) = buf.selection_range();
            if lo.row == hi.row {
                format!("{},{}-{}", lo.row + 1, lo.col + 1, hi.col + 1)
            } else {
                format!(
                    "{},{}-{},{}",
                    lo.row + 1,
                    lo.col + 1,
                    hi.row + 1,
                    hi.col + 1
                )
            }
        }
        _ => format!("{},{}", buf.cursor.row + 1, buf.cursor.col + 1),
    }
}

/// Per-mode key hint (kept to keys the editor actually honors).
fn mode_hint(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "i 插入 · v 选择 · u 撤销 · Enter 发送",
        Mode::Insert => "Enter 发送 · Esc 普通模式",
        Mode::Visual(_) => "y 复制 · d 删除 · Esc 取消",
    }
}

/// Display column of the cursor on its line: the summed display width of every
/// char before `buf.cursor.col` (CJK chars advance by 2).
fn cursor_disp_col(buf: &EditBuffer) -> usize {
    let line = match buf.lines.get(buf.cursor.row) {
        Some(l) => l,
        None => return 0,
    };
    line.chars()
        .take(buf.cursor.col)
        .map(|c| disp_width(&c.to_string()))
        .sum()
}

/// Pure render. `width`/`height` are the inline viewport dimensions; the last
/// row is the status line, so the body gets `height - 1` rows (at least 1).
/// Never panics for tiny width/height (mirrors `header.rs` robustness).
pub fn render(buf: &EditBuffer, width: u16, height: u16) -> RenderView {
    let body_h = (height.saturating_sub(1)).max(1) as usize;
    let len = buf.lines.len();

    // Scroll so the cursor row sits within [scroll, scroll + body_h). Start from
    // the buffer's own scroll, then clamp it up/down to keep the cursor visible.
    let mut scroll = buf.scroll;
    if buf.cursor.row < scroll {
        scroll = buf.cursor.row;
    } else if buf.cursor.row >= scroll + body_h {
        scroll = buf.cursor.row + 1 - body_h;
    }
    // Never scroll past the last possible top so we don't leave the window empty.
    let max_scroll = len.saturating_sub(body_h).min(buf.cursor.row);
    scroll = scroll.min(max_scroll);

    // Selection, only when in Visual mode.
    let sel = match buf.mode {
        Mode::Visual(_) => Some(buf.selection_range()),
        _ => None,
    };
    let kind = match buf.mode {
        Mode::Visual(k) => k,
        _ => VisualKind::Char, // unused when sel is None
    };

    let end = (scroll + body_h).min(len);
    let mut lines = Vec::with_capacity(end.saturating_sub(scroll));
    for (offset, line) in buf.lines[scroll..end].iter().enumerate() {
        let row = scroll + offset;
        let spans = selection_segments(line, row, sel.clone(), kind);
        lines.push(RenderLine { spans });
    }

    let cursor_row = buf.cursor.row.saturating_sub(scroll) as u16;
    let cursor_col = cursor_disp_col(buf) as u16;
    let _ = width; // body width is governed by the I/O layer's Paragraph clipping

    RenderView {
        lines,
        cursor_screen: (cursor_row, cursor_col),
        mode: buf.mode,
        badge: mode_label(buf.mode),
        pos: position_text(buf),
        hint: mode_hint(buf.mode).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modal::buffer::{Cursor, EditBuffer, Mode, VisualKind};

    #[test]
    fn cursor_screen_col_uses_display_width_for_cjk() {
        let mut b = EditBuffer::from_str("中a");
        b.cursor = Cursor { row: 0, col: 1 }; // before 'a', after 1 wide char
        let v = render(&b, 40, 5);
        assert_eq!(v.cursor_screen.0, 0); // row
        assert_eq!(v.cursor_screen.1, 2); // col: 中 = 2 display cols
    }

    #[test]
    fn status_shows_mode_and_position() {
        let b = EditBuffer::new();
        let v = render(&b, 40, 5);
        assert_eq!(v.badge, "NORMAL");
        assert_eq!(v.pos, "1,1");
        assert!(!v.hint.is_empty());
    }

    #[test]
    fn visual_position_shows_column_range() {
        let mut b = EditBuffer::from_str("abcdef");
        b.mode = Mode::Visual(VisualKind::Char);
        b.anchor = Some(Cursor { row: 0, col: 1 });
        b.cursor = Cursor { row: 0, col: 4 };
        let v = render(&b, 40, 5);
        assert_eq!(v.badge, "VISUAL");
        assert_eq!(v.pos, "1,2-5");
    }

    #[test]
    fn visual_selection_is_highlighted() {
        let mut b = EditBuffer::from_str("abcdef");
        b.mode = Mode::Visual(VisualKind::Char);
        b.anchor = Some(Cursor { row: 0, col: 0 });
        b.cursor = Cursor { row: 0, col: 2 }; // select abc
        let v = render(&b, 40, 5);
        // first body line has a highlighted "abc" span
        let hl: String = v.lines[0]
            .spans
            .iter()
            .filter(|(_, h)| *h)
            .map(|(s, _)| s.clone())
            .collect();
        assert_eq!(hl, "abc");
    }

    #[test]
    fn scroll_keeps_cursor_visible_when_over_height() {
        let body = (0..20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let mut b = EditBuffer::from_str(&body);
        b.cursor = Cursor { row: 19, col: 0 };
        let v = render(&b, 40, 5); // 5 rows total (incl status)
        assert!(v.cursor_screen.0 < 5);
        assert!(v.lines.len() <= 5);
    }

    // ---- Extra coverage beyond the plan's four tests. ----

    #[test]
    fn non_visual_line_is_single_plain_span() {
        let b = EditBuffer::from_str("hello");
        let v = render(&b, 40, 5);
        assert_eq!(v.lines[0].spans, vec![("hello".to_string(), false)]);
    }

    #[test]
    fn line_visual_highlights_whole_rows() {
        let mut b = EditBuffer::from_str("aa\nbb\ncc");
        b.mode = Mode::Visual(VisualKind::Line);
        b.anchor = Some(Cursor { row: 0, col: 1 });
        b.cursor = Cursor { row: 1, col: 0 }; // rows 0..=1 fully selected
        let v = render(&b, 40, 5);
        assert_eq!(v.lines[0].spans, vec![("aa".to_string(), true)]);
        assert_eq!(v.lines[1].spans, vec![("bb".to_string(), true)]);
        // Row 2 is outside the selection → plain.
        assert_eq!(v.lines[2].spans, vec![("cc".to_string(), false)]);
    }

    #[test]
    fn char_visual_multi_row_clips_first_and_last() {
        // Select from (0,1) to (2,1) inclusive over three rows.
        let mut b = EditBuffer::from_str("abc\ndef\nghi");
        b.mode = Mode::Visual(VisualKind::Char);
        b.anchor = Some(Cursor { row: 0, col: 1 });
        b.cursor = Cursor { row: 2, col: 1 };
        let v = render(&b, 40, 5);
        // First row: plain "a", highlight "bc".
        assert_eq!(
            v.lines[0].spans,
            vec![("a".to_string(), false), ("bc".to_string(), true)]
        );
        // Middle row: whole line highlighted.
        assert_eq!(v.lines[1].spans, vec![("def".to_string(), true)]);
        // Last row: highlight "gh" (cols 0..=1), then plain "i".
        assert_eq!(
            v.lines[2].spans,
            vec![("gh".to_string(), true), ("i".to_string(), false)]
        );
    }

    #[test]
    fn char_visual_cjk_boundary_safe() {
        // "中文字": select cols 0..=1 (中文) — must not split a CJK char.
        let mut b = EditBuffer::from_str("中文字");
        b.mode = Mode::Visual(VisualKind::Char);
        b.anchor = Some(Cursor { row: 0, col: 0 });
        b.cursor = Cursor { row: 0, col: 1 };
        let v = render(&b, 40, 5);
        let hl: String = v.lines[0]
            .spans
            .iter()
            .filter(|(_, h)| *h)
            .map(|(s, _)| s.clone())
            .collect();
        assert_eq!(hl, "中文");
    }

    #[test]
    fn scroll_window_is_clamped_to_buffer_end() {
        let body = (0..10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let mut b = EditBuffer::from_str(&body);
        b.cursor = Cursor { row: 9, col: 0 };
        // body_h = 3; with cursor at row 9 the window should be rows 7,8,9.
        let v = render(&b, 40, 4);
        assert_eq!(v.lines.len(), 3);
        assert_eq!(v.cursor_screen.0, 2); // 9 - 7
    }

    #[test]
    fn tiny_or_zero_dimensions_never_panic() {
        let mut b = EditBuffer::from_str("a\nb\nc");
        b.cursor = Cursor { row: 2, col: 0 };
        let _ = render(&b, 0, 0);
        let _ = render(&b, 0, 1);
        let _ = render(&b, 1, 1);
        // body_h clamps to >=1, so at least one line is produced.
        let v = render(&b, 1, 1);
        assert!(!v.lines.is_empty());
    }

    #[test]
    fn mode_labels_cover_all_modes() {
        assert_eq!(mode_label(Mode::Normal), "NORMAL");
        assert_eq!(mode_label(Mode::Insert), "INSERT");
        assert_eq!(mode_label(Mode::Visual(VisualKind::Char)), "VISUAL");
        assert_eq!(mode_label(Mode::Visual(VisualKind::Line)), "V-LINE");
    }
}
