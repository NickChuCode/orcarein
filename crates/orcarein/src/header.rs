//! Unified TUI header/chrome (v02-22): a bordered, icon'd welcome box that
//! replaces the old multi-line startup banner, plus a slim title bar shared by
//! overlay surfaces. Pure string rendering — no terminal I/O — so it is fully
//! unit-tested. See the v02-22 design spec.
//!
//! NOTE (v02-22, Tasks 1–4): the public API below is fully built and unit-tested
//! here, but the call sites that consume it live in the startup banner (Task 5)
//! and the overlay surfaces (Task 6). Until those land, the bin target sees these
//! items as dead code. The module-level allow keeps the lint clean during the
//! incremental build-up and is removed once the wiring tasks consume the API.
#![allow(dead_code)]

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

/// Plain `!`-prefixed warning lines, only for non-default states.
pub fn status_chips(no_permission: bool, economy_off: bool) -> Vec<String> {
    let mut v = Vec::new();
    if no_permission {
        v.push("! permissions disabled".to_string());
    }
    if economy_off {
        v.push("! cache economy OFF".to_string());
    }
    v
}

/// First 8 chars of a session id (or all, if shorter). Char-boundary safe.
pub fn short_id(id: &str) -> &str {
    match id.char_indices().nth(8) {
        Some((byte_idx, _)) => &id[..byte_idx],
        None => id,
    }
}

/// Collapse the user's home prefix in `p` to `~`. Testable core takes `home`.
pub fn abbreviate_home_with(home: &std::path::Path, p: &std::path::Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()).replace('\\', "/"),
        Err(_) => p.display().to_string(),
    }
}

/// Resolve home from the platform and abbreviate.
pub fn abbreviate_home(p: &std::path::Path) -> String {
    match directories::UserDirs::new() {
        Some(u) => abbreviate_home_with(u.home_dir(), p),
        None => p.display().to_string(),
    }
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

    #[test]
    fn status_chips_only_on_nondefault() {
        assert!(super::status_chips(false, false).is_empty());
        assert_eq!(super::status_chips(true, false).len(), 1);
        assert!(super::status_chips(true, false)[0].starts_with('!'));
        assert_eq!(super::status_chips(false, true).len(), 1);
        assert_eq!(super::status_chips(true, true).len(), 2);
    }

    #[test]
    fn short_id_takes_first_eight_or_all() {
        assert_eq!(super::short_id("0123456789abcdef"), "01234567");
        assert_eq!(super::short_id("abc"), "abc");
    }

    #[test]
    fn abbreviate_home_replaces_prefix() {
        // With an explicit home, the prefix collapses to `~`.
        let home = std::path::Path::new("/home/sarah");
        assert_eq!(
            super::abbreviate_home_with(home, std::path::Path::new("/home/sarah/p/x")),
            "~/p/x"
        );
        // Non-matching path is returned as-is.
        assert_eq!(
            super::abbreviate_home_with(home, std::path::Path::new("/etc/foo")),
            "/etc/foo"
        );
    }
}
