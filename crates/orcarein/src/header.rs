//! Unified TUI header/chrome (v02-22): a bordered, icon'd welcome box that
//! replaces the old multi-line startup banner, plus a slim title bar shared by
//! overlay surfaces. Pure string rendering — no terminal I/O — so it is fully
//! unit-tested. See the v02-22 design spec.

use unicode_width::UnicodeWidthStr;

/// Inline ASCII fish — 1 column per char, aligns on any terminal.
pub const APP_ICON: &str = "><(((°>";

/// Display width (CJK full-width counts as 2).
pub fn disp_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to at most `budget` display columns, never splitting a
/// multibyte/wide char. If it doesn't fit, the result ends with `…` and its
/// display width is `<= budget`.
pub fn truncate_to_width(s: &str, budget: usize) -> String {
    if disp_width(s) <= budget {
        return s.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    // Reserve 1 column for the ellipsis.
    let keep = budget - 1;
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = disp_width(&ch.to_string());
        if w + cw > keep {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disp_width_counts_cjk_as_two() {
        assert_eq!(disp_width("abc"), 3);
        assert_eq!(disp_width("中文"), 4);
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_ascii_adds_ellipsis_within_budget() {
        let out = truncate_to_width("abcdefgh", 5);
        assert!(out.ends_with('…'));
        assert!(disp_width(&out) <= 5);
    }

    #[test]
    fn truncate_never_splits_a_cjk_char() {
        // 6 columns of CJK; budget 5 -> must drop a whole char, stay valid UTF-8.
        let out = truncate_to_width("中文字", 5);
        assert!(disp_width(&out) <= 5);
        assert!(out.ends_with('…'));
        // Every char is whole (no panic above proves boundary safety).
        assert!(out.chars().all(|c| c == '中' || c == '文' || c == '字' || c == '…'));
    }

    #[test]
    fn truncate_zero_budget_is_empty() {
        assert_eq!(truncate_to_width("x", 0), "");
    }
}
