//! Unified TUI header/chrome (v02-24, redesigned per the Claude Design
//! "OrcaRein 终端设计系统"): a bordered welcome box with labeled identity, a
//! `getting started` command strip, and the model echoed in the title bar.
//! A full-size pixel whale banner is printed separately, above this box.
//! Pure rendering — no terminal I/O — so it is fully unit-tested.
//!
//! Color model: [`render_header`] builds a structured `Vec<Vec<Span>>` (text +
//! semantic [`Token`]). [`header_plain`] concatenates the text (the authoritative
//! width/golden surface — ANSI escapes are zero-width and must never enter the
//! layout math) and [`header_ansi`] paints each span via [`crate::color`]. The
//! slim title bar shared by overlay surfaces lives here too.

use crate::color::{self, ColorMode, Token};
use orcarein_core::PermissionMode;
use unicode_width::UnicodeWidthStr;

/// Box-drawing width thresholds.
pub const NARROW: u16 = 60;
pub const MIN_BOX_WIDTH: u16 = 24;

/// Fixed width of the identity label field.
const LABEL_W: usize = 8;

/// A run of text carrying one semantic color [`Token`]. Concatenating a line's
/// span texts yields the plain (uncolored) line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub token: Token,
}

fn sp(text: impl Into<String>, token: Token) -> Span {
    Span {
        text: text.into(),
        token,
    }
}

/// The header's input model. Fields are kept structured (not pre-joined) so each
/// piece can carry its own color.
pub struct HeaderModel<'a> {
    pub title: &'a str,
    pub model: &'a str,
    pub provider: &'a str,
    pub cwd: String,
    pub session: &'a str,
    pub saved: bool,
    /// `(command, 说明)` pairs for the getting-started strip.
    pub tips: Vec<(&'a str, &'a str)>,
}

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
    let keep = budget - 1; // reserve 1 column for the ellipsis
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

/// A single combined `!` warning line for non-default states, or `None`.
pub fn status_line(perm_mode: PermissionMode, economy_off: bool) -> Option<String> {
    let mut parts = Vec::new();
    match perm_mode {
        PermissionMode::Default => {}
        PermissionMode::AcceptEdits => parts.push("档位 acceptEdits".to_string()),
        PermissionMode::Plan => parts.push("档位 plan（只读）".to_string()),
        PermissionMode::Yolo => parts.push("YOLO：无确认、无回滚网".to_string()),
    }
    if economy_off {
        parts.push("缓存节流 OFF".to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("! {}", parts.join(" · ")))
    }
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

/// Pad `s` with spaces on the right to exactly `w` display columns (truncating
/// first if longer). Result has `disp_width == w`.
fn pad_to(s: &str, w: usize) -> String {
    let t = truncate_to_width(s, w);
    let gap = w.saturating_sub(disp_width(&t));
    format!("{t}{}", " ".repeat(gap))
}

/// A row of `c` repeated to `n` display columns (c assumed width-1).
fn rule(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

/// Fit colored `pieces` into `budget` display columns: keep pieces while they
/// fit, truncate the overflowing one, drop the rest; when `pad`, append trailing
/// spaces so the total is exactly `budget`. Without `pad` it just clips (the
/// one-liner). Result's total `disp_width` is `budget` (pad) or `<= budget`.
fn assemble(pieces: Vec<Span>, budget: usize, pad: bool) -> Vec<Span> {
    let mut used = 0usize;
    let mut out = Vec::new();
    for p in pieces {
        let w = disp_width(&p.text);
        if used + w <= budget {
            used += w;
            out.push(p);
        } else {
            let room = budget - used;
            if room > 0 {
                let t = truncate_to_width(&p.text, room);
                used += disp_width(&t);
                out.push(Span {
                    text: t,
                    token: p.token,
                });
            }
            break;
        }
    }
    if pad && used < budget {
        out.push(sp(" ".repeat(budget - used), Token::Fg));
    }
    out
}

/// Top border `╭─ <title> <fill> <echo> ─╮`, exactly `width` wide. `echo` is the
/// model (wide) or provider (narrow), truncated so a `>=2`-col fill remains.
fn top_border(title: &str, echo: &str, width: usize) -> Vec<Span> {
    let base = 3 + disp_width(title) + 1 + 1 + 3; // ╭─␠ title ␠ … ␠ echo ␠─╮
    let echo = truncate_to_width(echo, width.saturating_sub(base + 2));
    let fill = width.saturating_sub(base + disp_width(&echo));
    vec![
        sp("╭─ ", Token::Brand),
        sp(title, Token::OrcaWhite),
        sp(" ", Token::Brand),
        sp(rule('─', fill), Token::Brand),
        sp(" ", Token::Brand),
        sp(echo, Token::Accent),
        sp(" ─╮", Token::Brand),
    ]
}

/// Section divider `├─ <label> <fill>┤`, exactly `width` wide.
fn divider(label: &str, width: usize) -> Vec<Span> {
    let base = 3 + disp_width(label) + 1 + 1; // ├─␠ label ␠ … ┤
    let fill = width.saturating_sub(base);
    vec![
        sp("├─ ", Token::Brand),
        sp(label, Token::Accent),
        sp(" ", Token::Brand),
        sp(rule('─', fill), Token::Brand),
        sp("┤", Token::Brand),
    ]
}

/// Bottom border `╰<rule>╯`, `inner+2` wide.
fn bottom(inner: usize) -> Vec<Span> {
    vec![
        sp("╰", Token::Brand),
        sp(rule('─', inner), Token::Brand),
        sp("╯", Token::Brand),
    ]
}

/// The getting-started command strip (wide), `│ /cmd 说明 … │`.
fn cmd_strip(tips: &[(&str, &str)], inner: usize) -> Vec<Span> {
    let mut pieces = vec![sp(" ", Token::Dim)];
    for (idx, (name, desc)) in tips.iter().enumerate() {
        pieces.push(sp(*name, Token::Brand));
        pieces.push(sp(format!(" {desc}"), Token::Dim));
        if idx + 1 < tips.len() {
            pieces.push(sp("   ", Token::Dim));
        }
    }
    let mut line = vec![sp("│", Token::Brand)];
    line.extend(assemble(pieces, inner, true));
    line.push(sp("│", Token::Brand));
    line
}

fn render_double(m: &HeaderModel, inner: usize) -> Vec<Vec<Span>> {
    let width = inner + 2;
    let mut out = vec![top_border(m.title, m.model, width)];

    let row = |pieces: Vec<Span>| -> Vec<Span> {
        let mut line = vec![sp("│", Token::Brand)];
        line.extend(assemble(pieces, inner, true));
        line.push(sp("│", Token::Brand));
        line
    };
    let label = |name: &str| sp(format!(" {}", pad_to(name, LABEL_W)), Token::Dim);

    out.push(row(vec![
        label("model"),
        sp(m.model, Token::Accent),
        sp(format!(" · {}", m.provider), Token::Dim),
    ]));
    out.push(row(vec![label("cwd"), sp(m.cwd.clone(), Token::OrcaWhite)]));
    {
        let mut id = vec![label("session"), sp(m.session, Token::Fg)];
        if m.saved {
            id.push(sp(" · auto-saved", Token::Success));
        }
        out.push(row(id));
    }
    out.push(divider("getting started", width));
    out.push(cmd_strip(&m.tips, inner));
    out.push(bottom(inner));
    out
}

fn render_single(m: &HeaderModel, inner: usize) -> Vec<Vec<Span>> {
    let width = inner + 2;
    let mut out = vec![top_border(m.title, m.provider, width)];

    let row = |pieces: Vec<Span>| -> Vec<Span> {
        let mut line = vec![sp("│", Token::Brand)];
        line.extend(assemble(pieces, inner, true));
        line.push(sp("│", Token::Brand));
        line
    };
    let label = |name: &str| sp(format!(" {}", pad_to(name, LABEL_W)), Token::Dim);

    out.push(row(vec![label("model"), sp(m.model, Token::Accent)]));
    out.push(row(vec![label("cwd"), sp(m.cwd.clone(), Token::OrcaWhite)]));
    {
        let mut id = vec![label("session"), sp(m.session, Token::Fg)];
        if m.saved {
            id.push(sp(" ·saved", Token::Success));
        }
        out.push(row(id));
    }
    out.push(divider("getting started", width));
    for (name, desc) in &m.tips {
        let pad = 9usize.saturating_sub(disp_width(name));
        out.push(row(vec![
            sp(" ", Token::Dim),
            sp(*name, Token::Brand),
            sp(" ".repeat(pad), Token::Dim),
            sp(*desc, Token::Dim),
        ]));
    }
    out.push(bottom(inner));
    out
}

fn one_liner(m: &HeaderModel, width: usize) -> Vec<Span> {
    let pieces = vec![
        sp(m.title, Token::OrcaWhite),
        sp(" · ", Token::Dim),
        sp(m.model, Token::Accent),
        sp(" · ", Token::Dim),
        sp(m.provider, Token::Fg),
        sp(" · ", Token::Dim),
        sp(m.cwd.clone(), Token::Fg),
        sp(" · ", Token::Dim),
        sp("/help", Token::Brand),
    ];
    assemble(pieces, width, false)
}

/// Render the header into structured spans. Three tiers: a full box (`>= NARROW`),
/// a narrow single-column box (`>= MIN_BOX_WIDTH`), or a one-line summary
/// (`!fancy` or tiny width).
pub fn render_header(m: &HeaderModel, width: u16, fancy: bool) -> Vec<Vec<Span>> {
    if !fancy || width < MIN_BOX_WIDTH {
        return vec![one_liner(m, width as usize)];
    }
    let inner = (width - 2) as usize;
    if width < NARROW {
        render_single(m, inner)
    } else {
        render_double(m, inner)
    }
}

/// Concatenate each line's span texts into the plain (uncolored) line. This is
/// the authoritative width/golden surface.
pub fn header_plain(lines: &[Vec<Span>]) -> Vec<String> {
    lines
        .iter()
        .map(|spans| spans.iter().map(|s| s.text.as_str()).collect())
        .collect()
}

/// Paint each span via [`crate::color`]. With [`ColorMode::None`] this is exactly
/// [`header_plain`] (identity passthrough — same fast path, no escapes emitted).
pub fn header_ansi(lines: &[Vec<Span>], mode: ColorMode) -> Vec<String> {
    if mode == ColorMode::None {
        return header_plain(lines);
    }
    lines
        .iter()
        .map(|spans| {
            let mut out = String::new();
            for s in spans {
                out.push_str(&color::paint(mode, s.token, &s.text));
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

    /// Strip SGR escapes so painted output can be compared to the plain text.
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for d in chars.by_ref() {
                    if d == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn demo_model() -> HeaderModel<'static> {
        HeaderModel {
            title: "OrcaRein",
            model: "deepseek-v4-flash",
            provider: "deepseek",
            cwd: "~/projects/foo".to_string(),
            session: "0a1b2c3d",
            saved: true,
            tips: vec![
                ("/help", "命令一览"),
                ("/init", "初始化"),
                ("/compact", "压缩上下文"),
            ],
        }
    }

    fn plain(m: &HeaderModel, width: u16, fancy: bool) -> Vec<String> {
        header_plain(&render_header(m, width, fancy))
    }

    #[test]
    fn disp_width_counts_cjk_as_two() {
        assert_eq!(disp_width("abc"), 3);
        assert_eq!(disp_width("中文"), 4);
    }

    #[test]
    fn truncate_keeps_short_and_ellipsizes_long() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        let out = truncate_to_width("abcdefgh", 5);
        assert!(out.ends_with('…') && disp_width(&out) <= 5);
        assert_eq!(truncate_to_width("x", 0), "");
    }

    #[test]
    fn truncate_never_splits_a_cjk_char() {
        let out = truncate_to_width("中文字", 5);
        assert!(disp_width(&out) <= 5 && out.ends_with('…'));
        assert!(out.chars().all(|c| "中文字…".contains(c)));
    }

    #[test]
    fn double_col_golden() {
        let lines = plain(&demo_model(), 100, true);
        let expected = vec![
            "╭─ OrcaRein ─────────────────────────────────────────────────────────────────── deepseek-v4-flash ─╮",
            "│ model   deepseek-v4-flash · deepseek                                                             │",
            "│ cwd     ~/projects/foo                                                                           │",
            "│ session 0a1b2c3d · auto-saved                                                                    │",
            "├─ getting started ────────────────────────────────────────────────────────────────────────────────┤",
            "│ /help 命令一览   /init 初始化   /compact 压缩上下文                                              │",
            "╰──────────────────────────────────────────────────────────────────────────────────────────────────╯",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn single_col_golden() {
        let lines = plain(&demo_model(), 40, true);
        let expected = vec![
            "╭─ OrcaRein ──────────────── deepseek ─╮",
            "│ model   deepseek-v4-flash            │",
            "│ cwd     ~/projects/foo               │",
            "│ session 0a1b2c3d ·saved              │",
            "├─ getting started ────────────────────┤",
            "│ /help    命令一览                    │",
            "│ /init    初始化                      │",
            "│ /compact 压缩上下文                  │",
            "╰──────────────────────────────────────╯",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn double_col_invariants() {
        let lines = plain(&demo_model(), 100, true);
        for l in &lines {
            assert_eq!(disp_width(l), 100, "every line must fill the width: {l:?}");
        }
        assert!(lines.first().unwrap().starts_with('╭') && lines.first().unwrap().ends_with('╮'));
        assert!(lines.last().unwrap().starts_with('╰') && lines.last().unwrap().ends_with('╯'));
        assert!(lines.iter().any(|l| l.starts_with('├') && l.ends_with('┤')));
        assert!(lines.iter().any(|l| l.contains("deepseek-v4-flash")));
        assert!(lines.iter().any(|l| l.contains("/compact")));
    }

    #[test]
    fn one_liner_when_not_fancy() {
        let lines = plain(&demo_model(), 100, false);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains('╭'));
        assert!(lines[0].contains("OrcaRein") && lines[0].contains("/help"));
        assert!(lines[0].contains("deepseek-v4-flash"));
    }

    #[test]
    fn tiny_or_zero_width_never_panics() {
        let _ = plain(&demo_model(), 0, true);
        let _ = plain(&demo_model(), 10, true); // < MIN_BOX_WIDTH -> one-liner
    }

    #[test]
    fn cjk_cwd_keeps_width() {
        let mut m = demo_model();
        m.cwd = "~/项目/中文".to_string();
        for l in plain(&m, 60, true) {
            assert_eq!(disp_width(&l), 60);
        }
        for l in plain(&m, 100, true) {
            assert_eq!(disp_width(&l), 100);
        }
    }

    #[test]
    fn double_col_at_narrow_boundary_does_not_panic() {
        for l in plain(&demo_model(), 60, true) {
            assert_eq!(disp_width(&l), 60);
        }
    }

    #[test]
    fn ansi_strips_back_to_plain_and_colors_key_spans() {
        let spans = render_header(&demo_model(), 100, true);
        let plain = header_plain(&spans);
        let painted = header_ansi(&spans, ColorMode::Truecolor);
        // Painted, with escapes removed, equals the plain golden.
        for (p, c) in plain.iter().zip(painted.iter()) {
            assert_eq!(&strip_sgr(c), p);
        }
        // Title is orca-white; the model echo is accent; both appear painted.
        assert!(painted[0].contains(&color::paint(
            ColorMode::Truecolor,
            Token::OrcaWhite,
            "OrcaRein"
        )));
        assert!(painted[0].contains(&color::paint(
            ColorMode::Truecolor,
            Token::Accent,
            "deepseek-v4-flash"
        )));
    }

    #[test]
    fn ansi_none_equals_plain() {
        let spans = render_header(&demo_model(), 100, true);
        assert_eq!(header_ansi(&spans, ColorMode::None), header_plain(&spans));
    }

    #[test]
    fn status_line_only_on_nondefault() {
        assert_eq!(status_line(PermissionMode::Default, false), None);
        assert_eq!(
            status_line(PermissionMode::Default, true).unwrap(),
            "! 缓存节流 OFF"
        );
        assert_eq!(
            status_line(PermissionMode::Yolo, true).unwrap(),
            "! YOLO：无确认、无回滚网 · 缓存节流 OFF"
        );
    }

    #[test]
    fn status_line_shows_accept_edits_mode() {
        assert_eq!(
            status_line(PermissionMode::AcceptEdits, false).unwrap(),
            "! 档位 acceptEdits"
        );
    }

    #[test]
    fn status_line_shows_plan_mode() {
        assert_eq!(
            status_line(PermissionMode::Plan, false).unwrap(),
            "! 档位 plan（只读）"
        );
    }

    #[test]
    fn status_line_shows_yolo_mode() {
        assert_eq!(
            status_line(PermissionMode::Yolo, false).unwrap(),
            "! YOLO：无确认、无回滚网"
        );
    }

    #[test]
    fn short_id_takes_first_eight_or_all() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn abbreviate_home_replaces_prefix() {
        let home = std::path::Path::new("/home/sarah");
        assert_eq!(
            abbreviate_home_with(home, std::path::Path::new("/home/sarah/p/x")),
            "~/p/x"
        );
        assert_eq!(
            abbreviate_home_with(home, std::path::Path::new("/etc/foo")),
            "/etc/foo"
        );
    }

    #[test]
    fn slim_title_bar_fills_width_with_corners() {
        let bar = slim_title_bar("对话记录", 40);
        assert_eq!(disp_width(&bar), 40);
        assert!(bar.starts_with('╭') && bar.ends_with('╮'));
        assert!(bar.contains("对话记录"));
    }
}
