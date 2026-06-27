//! Unified TUI header/chrome (v02-22): a bordered, icon'd welcome box that
//! replaces the old multi-line startup banner, plus a slim title bar shared by
//! overlay surfaces. Pure string rendering — no terminal I/O — so it is fully
//! unit-tested. See the v02-22 design spec.

use unicode_width::UnicodeWidthStr;

/// Pixel-art whale mascot (faceless, traced from the project logo): a plump
/// whale with its head to the left and tail flukes flicking up at the right.
/// Rendered centered at the top of the left column in the double-column header.
/// Every line is a verified, equal display width (`MASCOT_W`); only width-1
/// solid-block glyphs are used — no box-drawing chars, which [`paint_borders`]
/// would dye blue — so the box never skews and the silhouette stays uncolored.
pub const MASCOT: &[&str] = &[
    "                ▗▟▘",
    "  ▄▄▄▄▄▄       ▄▟▛ ",
    "▗█████████▄▄▄▟██▛  ",
    "▟███████████████▖  ",
    "▜██████████████▛   ",
    " ▝▀▀████████▀▀     ",
];

/// Display width of every [`MASCOT`] line (all lines share this width).
pub const MASCOT_W: usize = 19;

/// The box-drawing glyphs we paint when coloring the border.
const BORDER_GLYPHS: &[char] = &['╭', '╮', '╰', '╯', '─', '│', '┬', '┴'];

/// DeepSeek blue (#4D6BFE) truecolor SGR prefix, paired with the reset below.
const BLUE: &str = "\x1b[38;2;77;107;254m";
const RESET: &str = "\x1b[0m";

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

/// Center `s` within `w` display columns by padding both sides with spaces
/// (extra column, if any, goes to the right). Truncates first if longer, so the
/// result always has `disp_width == w`.
fn center_to(s: &str, w: usize) -> String {
    let t = truncate_to_width(s, w);
    let pad = w.saturating_sub(disp_width(&t));
    let lhs = pad / 2;
    let rhs = pad - lhs;
    format!("{}{}{}", " ".repeat(lhs), t, " ".repeat(rhs))
}

pub fn render_header(m: &HeaderModel, width: u16, fancy: bool) -> Vec<String> {
    if !fancy || width < MIN_BOX_WIDTH {
        let summary = format!(
            "{} · {} · /help",
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
    let label = truncate_to_width(m.title, inner.saturating_sub(3));
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
    // top line: ╭─ OrcaRein ─...─┬─ getting started ─...─╮
    // l_seg = "─ {l_label} " has width W(l_label)+3; need it <= left so the
    // fill never underflows and disp_width(l_top) == left. So budget = left-3.
    let l_label = truncate_to_width(m.title, left.saturating_sub(3));
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

    // Left column = mascot lines (centered, only when it fits) + identity rows.
    let mut left_cells: Vec<String> = Vec::new();
    if MASCOT_W <= left {
        for art in MASCOT {
            left_cells.push(center_to(art, left));
        }
    }
    for (_, v) in &m.identity {
        left_cells.push(pad_to(v, left));
    }
    // Right column = tips.
    let right_cells: Vec<String> = m
        .tips
        .iter()
        .map(|(k, d)| pad_to(&format!("{k}  {d}"), right))
        .collect();

    // body rows: zip the two columns, padding the shorter with blank cells.
    let rows = left_cells.len().max(right_cells.len());
    let blank_l = pad_to("", left);
    let blank_r = pad_to("", right);
    for i in 0..rows {
        let lc = left_cells.get(i).unwrap_or(&blank_l);
        let rc = right_cells.get(i).unwrap_or(&blank_r);
        out.push(format!("│{lc}│{rc}│"));
    }
    out.push(format!("╰{}┴{}╯", rule('─', left), rule('─', right)));
    out
}

/// Wrap the box-drawing border glyphs of each line in the DeepSeek-blue SGR
/// escape (content/mascot/text left untouched), when `enabled`. With `enabled`
/// false this is a passthrough — identical strings — so the uncolored
/// `render_header` output (and its width tests/goldens) stay authoritative.
/// Coloring is applied *after* layout, so the escape sequences never enter the
/// `disp_width` math that built the box.
pub fn paint_borders(lines: &[String], enabled: bool) -> Vec<String> {
    if !enabled {
        return lines.to_vec();
    }
    lines
        .iter()
        .map(|line| {
            let mut out = String::with_capacity(line.len() + 16);
            let mut in_span = false;
            for ch in line.chars() {
                let is_border = BORDER_GLYPHS.contains(&ch);
                if is_border && !in_span {
                    out.push_str(BLUE);
                    in_span = true;
                } else if !is_border && in_span {
                    out.push_str(RESET);
                    in_span = false;
                }
                out.push(ch);
            }
            if in_span {
                out.push_str(RESET);
            }
            out
        })
        .collect()
}

/// A single-line title bar `╭─ <title> ─…─╮` exactly `width` wide.
pub fn slim_title_bar(title: &str, width: u16) -> String {
    let w = width as usize;
    if w < 4 {
        return rule('─', w);
    }
    let inner = w - 2; // corners
    let label = truncate_to_width(title, inner.saturating_sub(2));
    let seg = format!("─ {label} ");
    let fill = rule('─', inner.saturating_sub(disp_width(&seg)));
    format!("╭{seg}{fill}╮")
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
        // The inline fish is retired everywhere.
        assert!(!lines[0].contains("><((("));
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
            "╭─ OrcaRein ──────────────────────────────────────────┬ getting started ───────────────────────────╮",
            "│                                 ▗▟▘                 │/help  commands                             │",
            "│                   ▄▄▄▄▄▄       ▄▟▛                  │/init  make AGENTS.md                       │",
            "│                 ▗█████████▄▄▄▟██▛                   │/compact  shrink context                    │",
            "│                 ▟███████████████▖                   │                                            │",
            "│                 ▜██████████████▛                    │                                            │",
            "│                  ▝▀▀████████▀▀                      │                                            │",
            "│deepseek-v4-flash · deepseek                         │                                            │",
            "│~/projects/foo                                       │                                            │",
            "│0a1b2c3d · auto-saved                                │                                            │",
            "╰─────────────────────────────────────────────────────┴────────────────────────────────────────────╯",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn single_col_golden() {
        let lines = render_header(&demo_model(), 40, true);
        let expected = vec![
            "╭─ OrcaRein ───────────────────────────╮",
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

    #[test]
    fn slim_title_bar_fills_width_with_corners() {
        let bar = super::slim_title_bar("对话记录", 40);
        assert_eq!(disp_width(&bar), 40);
        assert!(bar.starts_with('╭'));
        assert!(bar.ends_with('╮'));
        assert!(bar.contains("对话记录"));
    }

    #[test]
    fn mascot_lines_all_equal_width() {
        // Load-bearing: any width-2 or ragged glyph would skew the left column.
        assert!(!MASCOT.is_empty());
        for l in MASCOT {
            assert_eq!(
                disp_width(l),
                MASCOT_W,
                "mascot line not {MASCOT_W} wide: {l:?}"
            );
        }
    }

    #[test]
    fn double_col_shows_mascot_above_identity() {
        let lines = render_header(&demo_model(), 100, true);
        // The whale's solid body row appears in some body line's left cell,
        // and it sits above the first identity value.
        let mascot_row = lines.iter().position(|l| l.contains("██████████████"));
        let id_row = lines.iter().position(|l| l.contains("deepseek-v4-flash"));
        assert!(mascot_row.is_some(), "mascot missing from double-col box");
        assert!(id_row.is_some());
        assert!(
            mascot_row.unwrap() < id_row.unwrap(),
            "mascot must be above identity"
        );
    }

    #[test]
    fn paint_borders_disabled_is_passthrough() {
        let lines = render_header(&demo_model(), 100, true);
        assert_eq!(paint_borders(&lines, false), lines);
    }

    #[test]
    fn paint_borders_wraps_only_border_glyphs() {
        let lines = render_header(&demo_model(), 100, true);
        let painted = paint_borders(&lines, true);
        const BLUE: &str = "\x1b[38;2;77;107;254m";
        const RESET: &str = "\x1b[0m";
        // Top line starts with a colored corner.
        assert!(painted[0].starts_with(&format!("{BLUE}╭")));
        // The colored top line carries the product name uncolored: the blue
        // escape never sits immediately before a content char like 'O'.
        assert!(painted[0].contains("OrcaRein"));
        assert!(
            !painted[0].contains(&format!("{BLUE}O")),
            "title text must not be colored"
        );
        // Find the body line carrying the model value and verify its content is
        // not inside a color span, while its leading border is.
        let (i, body) = painted
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains("deepseek-v4-flash"))
            .expect("model line present");
        assert!(
            !body.contains(&format!("{BLUE}d")),
            "content must not be colored"
        );
        // The reset closes the leading border before content begins.
        assert!(body.starts_with(&format!("{BLUE}│{RESET}")));
        // Original visible text survives (strip escapes → equals plain line).
        let stripped: String = body.replace(BLUE, "").replace(RESET, "");
        assert_eq!(stripped, lines[i]);
    }
}
