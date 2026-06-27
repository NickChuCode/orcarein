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

pub const NARROW: u16 = 60;
pub const MIN_BOX_WIDTH: u16 = 24;
const MIN_LEFT: usize = 16;
const MIN_RIGHT: usize = 16;

pub struct HeaderModel<'a> {
    pub icon: &'a str,
    pub title: &'a str,
    pub identity: Vec<(&'a str, String)>,
    pub tips: Vec<(&'a str, &'a str)>,
}

/// Pad `s` with spaces on the right to exactly `w` display columns (truncating
/// first if longer). Result has `disp_width == w`.
fn pad_to(s: &str, w: usize) -> String {
    let t = truncate_to_width(s, w);
    let mut out = t;
    let gap = w.saturating_sub(disp_width(&out));
    out.push_str(&" ".repeat(gap));
    out
}

/// A row of `c` repeated to `n` display columns (c assumed width-1).
fn rule(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

pub fn render_header(m: &HeaderModel, width: u16, fancy: bool) -> Vec<String> {
    if !fancy || width < MIN_BOX_WIDTH {
        let summary = format!(
            "{} {} · {} · /help",
            m.icon,
            m.title,
            m.identity.first().map(|(_, v)| v.as_str()).unwrap_or("")
        );
        return vec![truncate_to_width(&summary, width as usize)];
    }
    let inner = width.saturating_sub(2) as usize;
    if width < NARROW {
        return render_single_col(m, inner);
    }
    render_double_col(m, inner)
}

fn render_single_col(m: &HeaderModel, inner: usize) -> Vec<String> {
    let mut out = Vec::new();
    // top: "╭─ {label} " + fill('─') + "╮", total disp_width == inner+2.
    // Visible: ╭(1) ─(1) space(1) label space(1) fill ╮(1) = 5 + W(label) + fill.
    // Want 5 + W(label) + fill == inner+2  =>  fill = inner - W(label) - 3.
    // Truncate label to inner-3 so fill never underflows.
    let label = truncate_to_width(&format!("{} {}", m.icon, m.title), inner.saturating_sub(3));
    let fill = inner.saturating_sub(disp_width(&label) + 3);
    out.push(format!("╭─ {label} {}╮", rule('─', fill)));
    for (k, v) in &m.identity {
        out.push(format!("│{}│", pad_to(&format!("{k}  {v}"), inner)));
    }
    out.push(format!("│{}│", pad_to("", inner)));
    for (k, d) in &m.tips {
        out.push(format!("│{}│", pad_to(&format!("{k}  {d}"), inner)));
    }
    out.push(format!("╰{}╯", rule('─', inner)));
    out
}

fn render_double_col(m: &HeaderModel, inner: usize) -> Vec<String> {
    let hi = inner.saturating_sub(MIN_RIGHT + 1);
    let left = if hi < MIN_LEFT {
        hi
    } else {
        (inner * 55 / 100).clamp(MIN_LEFT, hi)
    };
    let right = inner.saturating_sub(left + 1);

    let mut out = Vec::new();
    // top line: ╭─ icon title ─...─┬─ getting started ─...─╮
    // l_seg = "─ {l_label} " has width W(l_label)+3; need it <= left so the
    // fill never underflows and disp_width(l_top) == left. So budget = left-3.
    let l_label = truncate_to_width(&format!("{} {}", m.icon, m.title), left.saturating_sub(3));
    let l_seg = format!("─ {l_label} ");
    let l_top = format!(
        "{}{}",
        l_seg,
        rule('─', left.saturating_sub(disp_width(&l_seg)))
    );
    let r_label = truncate_to_width(" getting started ", right);
    let r_top = format!(
        "{}{}",
        r_label,
        rule('─', right.saturating_sub(disp_width(&r_label)))
    );
    out.push(format!("╭{l_top}┬{r_top}╮"));

    // body rows: zip identity (left) with tips (right), padding the shorter.
    let rows = m.identity.len().max(m.tips.len());
    for i in 0..rows {
        let lc = m
            .identity
            .get(i)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let rc = m
            .tips
            .get(i)
            .map(|(k, d)| format!("{k}  {d}"))
            .unwrap_or_default();
        out.push(format!("│{}│{}│", pad_to(&lc, left), pad_to(&rc, right)));
    }
    out.push(format!("╰{}┴{}╯", rule('─', left), rule('─', right)));
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
        assert!(out
            .chars()
            .all(|c| c == '中' || c == '文' || c == '字' || c == '…'));
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

    fn demo_model() -> HeaderModel<'static> {
        HeaderModel {
            icon: APP_ICON,
            title: "OrcaRein",
            identity: vec![
                ("model", "deepseek-v4-flash · deepseek".to_string()),
                ("cwd", "~/projects/foo".to_string()),
                ("session", "0a1b2c3d · auto-saved".to_string()),
            ],
            tips: vec![
                ("/help", "commands"),
                ("/init", "make AGENTS.md"),
                ("/compact", "shrink context"),
            ],
        }
    }

    fn divider_col(line: &str) -> Option<usize> {
        // The interior divider (┬ on the top, │ on body rows, ┴ on the bottom).
        // Skip the outer border columns: the leading ╭/│/╰ at col 0 and the
        // trailing ╮/│/╯ at the last column. We measure display columns so CJK
        // body cells don't shift the reported position.
        let chars: Vec<char> = line.chars().collect();
        let last = chars.len().saturating_sub(1);
        let mut col = 0usize;
        for (i, c) in chars.iter().enumerate() {
            if (*c == '┬' || *c == '│' || *c == '┴') && i != 0 && i != last {
                return Some(col);
            }
            col += disp_width(&c.to_string());
        }
        None
    }

    #[test]
    fn double_col_invariants() {
        let m = demo_model();
        let lines = render_header(&m, 100, true);
        assert!(lines.len() >= 3);
        for l in &lines {
            assert_eq!(disp_width(l), 100, "every line must fill the width: {l:?}");
        }
        assert!(lines.first().unwrap().starts_with('╭'));
        assert!(lines.first().unwrap().ends_with('╮'));
        assert!(lines.last().unwrap().starts_with('╰'));
        assert!(lines.last().unwrap().ends_with('╯'));
        // ┬ / │ / ┴ all sit at the same column.
        let col = divider_col(&lines[0]).unwrap();
        for l in &lines {
            assert_eq!(divider_col(l), Some(col), "divider must align: {l:?}");
        }
        // content present.
        assert!(lines.iter().any(|l| l.contains("deepseek-v4-flash")));
        assert!(lines.iter().any(|l| l.contains("/compact")));
    }

    #[test]
    fn single_col_below_narrow() {
        let lines = render_header(&demo_model(), 40, true);
        for l in &lines {
            assert_eq!(disp_width(l), 40);
        }
        assert!(!lines.iter().any(|l| l.contains('┬')));
        assert!(lines.iter().any(|l| l.contains("~/projects/foo")));
    }

    #[test]
    fn plain_one_line_when_not_fancy() {
        let lines = render_header(&demo_model(), 100, false);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains('╭'));
        assert!(lines[0].contains("OrcaRein"));
        assert!(lines[0].contains("/help"));
    }

    #[test]
    fn tiny_or_zero_width_never_panics() {
        let _ = render_header(&demo_model(), 0, true);
        let _ = render_header(&demo_model(), 10, true); // < MIN_BOX_WIDTH -> one-liner
    }

    #[test]
    fn cjk_cwd_keeps_width() {
        let mut m = demo_model();
        m.identity[1].1 = "~/项目/中文".to_string();
        for l in render_header(&m, 60, true) {
            assert_eq!(disp_width(&l), 60);
        }
        for l in render_header(&m, 100, true) {
            assert_eq!(disp_width(&l), 100);
        }
    }

    #[test]
    fn double_col_at_narrow_boundary_does_not_panic() {
        for l in render_header(&demo_model(), 60, true) {
            assert_eq!(disp_width(&l), 60);
        }
    }

    #[test]
    fn double_col_golden() {
        let lines = render_header(&demo_model(), 100, true);
        let expected = vec![
            "╭─ ><(((°> OrcaRein ──────────────────────────────────┬ getting started ───────────────────────────╮",
            "│deepseek-v4-flash · deepseek                         │/help  commands                             │",
            "│~/projects/foo                                       │/init  make AGENTS.md                       │",
            "│0a1b2c3d · auto-saved                                │/compact  shrink context                    │",
            "╰─────────────────────────────────────────────────────┴────────────────────────────────────────────╯",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn single_col_golden() {
        let lines = render_header(&demo_model(), 40, true);
        let expected = vec![
            "╭─ ><(((°> OrcaRein ───────────────────╮",
            "│model  deepseek-v4-flash · deepseek   │",
            "│cwd  ~/projects/foo                   │",
            "│session  0a1b2c3d · auto-saved        │",
            "│                                      │",
            "│/help  commands                       │",
            "│/init  make AGENTS.md                 │",
            "│/compact  shrink context              │",
            "╰──────────────────────────────────────╯",
        ];
        assert_eq!(lines, expected);
    }
}
