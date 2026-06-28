//! Semantic ANSI color palette for the terminal UI, per the Claude Design
//! "OrcaRein 终端设计系统" (v02-24). Pure logic — no terminal I/O — so it is
//! fully unit-tested and ALWAYS compiled (even under `--no-default-features`,
//! which drops ratatui; the ratatui [`rt`] adapter is the only `tui`-gated piece
//! and lands with the modal/pager surfaces).
//!
//! Single source of truth: each [`Token`] carries its truecolor RGB plus the
//! 256-color and 16-color SGR fallbacks the design specified. [`paint`] emits the
//! right SGR for the active [`ColorMode`]; [`ColorMode::None`] is an identity
//! passthrough, so display-width math and goldens stay authoritative on the
//! UNCOLORED text (ANSI escapes are zero-width and must never enter that math).

/// A semantic color role. Concrete values live in [`Token::spec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// DeepSeek blue: frames, brand name, orca silhouette, NORMAL mode, `[回复]`.
    Brand,
    /// Teal accent: title-bar emphasis, V-LINE mode, meters, current/selection.
    Accent,
    /// Highest contrast: titles, orca belly highlight, key values (path/model).
    OrcaWhite,
    /// Secondary text: field labels, sub-rules, hints, `[思考]`.
    Dim,
    /// INSERT mode, `[result]` ok, permission y/A, `auto-saved`.
    Success,
    /// VISUAL mode, `!` warning rows, search-hit ground, default option.
    Warning,
    /// `[tool error]`, permission never, over-limit meter, fatal notices.
    Error,
    /// Body text, streamed answer, tool args; the only color kept under NO_COLOR.
    Fg,
}

impl Token {
    /// `(r, g, b, ansi256, ansi16_fg_sgr)` — the design's three-tier values.
    /// The 16-color entry is the final SGR foreground number (e.g. 94 = bright
    /// blue, 37 = white), not the palette index.
    const fn spec(self) -> (u8, u8, u8, u8, u16) {
        match self {
            Token::Brand => (77, 107, 254, 63, 94),
            Token::Accent => (43, 212, 196, 44, 96),
            Token::OrcaWhite => (234, 240, 250, 255, 97),
            Token::Dim => (91, 107, 143, 102, 90),
            Token::Success => (70, 196, 110, 71, 92),
            Token::Warning => (230, 180, 80, 179, 93),
            Token::Error => (255, 91, 91, 203, 91),
            Token::Fg => (198, 208, 224, 252, 37),
        }
    }
}

/// Terminal color capability, resolved once from the environment via [`detect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit `38;2;r;g;b`.
    Truecolor,
    /// 256-color `38;5;n`.
    Ansi256,
    /// 16-color `3x` / `9x`.
    Ansi16,
    /// No color — identity passthrough (NO_COLOR, non-tty, dumb terminal).
    None,
}

const RESET: &str = "\x1b[0m";

/// The SGR sequence that opens `token`'s foreground for `mode` (empty for None).
fn fg_open(token: Token, mode: ColorMode) -> String {
    let (r, g, b, c256, c16) = token.spec();
    match mode {
        ColorMode::Truecolor => format!("\x1b[38;2;{r};{g};{b}m"),
        ColorMode::Ansi256 => format!("\x1b[38;5;{c256}m"),
        ColorMode::Ansi16 => format!("\x1b[{c16}m"),
        ColorMode::None => String::new(),
    }
}

/// Wrap `text` in `token`'s foreground for `mode`. `None` returns `text`
/// unchanged, so width math / goldens computed on the plain string remain valid.
pub fn paint(mode: ColorMode, token: Token, text: &str) -> String {
    if mode == ColorMode::None {
        return text.to_string();
    }
    format!("{}{}{}", fg_open(token, mode), text, RESET)
}

/// Wrap `text` in reverse video (SGR 7). Used where a background color isn't
/// expressed as a token — the permission default option, and as the NO_COLOR
/// fallback for status bars. Reverse is an attribute, not a color, so the design
/// keeps it even under NO_COLOR.
pub fn reverse(text: &str) -> String {
    format!("\x1b[7m{text}{RESET}")
}

/// Resolve the color mode from the live environment. `is_tty` is whether the
/// relevant stream is an interactive terminal (color is suppressed otherwise so
/// redirected/piped output stays plain). The env reads are split out into the
/// pure [`classify`] so the decision table is unit-tested without touching the
/// process environment.
pub fn detect(is_tty: bool) -> ColorMode {
    let colorterm = std::env::var("COLORTERM").ok();
    let term = std::env::var("TERM").ok();
    classify(
        is_tty,
        std::env::var_os("NO_COLOR").is_some(),
        colorterm.as_deref(),
        term.as_deref(),
    )
}

/// Pure color-capability decision (see [`detect`]). Order: non-tty / NO_COLOR →
/// None; `COLORTERM` truecolor/24bit → Truecolor; then by `TERM`: unset →
/// Truecolor (Windows / modern terminals leave it unset yet support truecolor,
/// matching the prior hard-coded behavior), `dumb` → None, contains `256` →
/// Ansi256, otherwise → Ansi16.
fn classify(
    is_tty: bool,
    no_color: bool,
    colorterm: Option<&str>,
    term: Option<&str>,
) -> ColorMode {
    if !is_tty || no_color {
        return ColorMode::None;
    }
    if let Some(ct) = colorterm {
        let ct = ct.to_ascii_lowercase();
        if ct.contains("truecolor") || ct.contains("24bit") {
            return ColorMode::Truecolor;
        }
    }
    match term {
        None => ColorMode::Truecolor,
        Some(t) if t.eq_ignore_ascii_case("dumb") => ColorMode::None,
        Some(t) if t.to_ascii_lowercase().contains("256") => ColorMode::Ansi256,
        Some(_) => ColorMode::Ansi16,
    }
}

/// ratatui adapter: a [`Token`]'s truecolor `Color` for the modal editor and
/// pager (ratatui downgrades it per terminal). ANSI strings can't cross
/// ratatui's buffer, so those surfaces style via `ratatui::Style`, not [`paint`].
#[cfg(feature = "tui")]
pub fn rt(token: Token) -> ratatui::style::Color {
    let (r, g, b, _, _) = token.spec();
    ratatui::style::Color::Rgb(r, g, b)
}

/// Map an ANSI SGR foreground code (`30`–`37` / `90`–`97`) to its 0–15 palette
/// index. [`Token::spec`] stores the 16-color tier as the final SGR number; a
/// ratatui `Color::Indexed` wants the palette index, so bright codes `9x` map to
/// `8..=15` and normal codes `3x` to `0..=7`.
#[cfg(feature = "tui")]
const fn sgr_fg_to_index(sgr: u16) -> u8 {
    if sgr >= 90 {
        (sgr - 90 + 8) as u8
    } else {
        (sgr - 30) as u8
    }
}

/// The mode-aware sibling of [`rt`]: tiers `token` explicitly for `mode` on a
/// ratatui surface — truecolor → `Rgb`, 256 → `Indexed` (design 256 tier), 16 →
/// `Indexed` (palette index of the design SGR code). `None` returns the truecolor
/// `Rgb`; callers that distinguish NO_COLOR gate it before calling.
#[cfg(feature = "tui")]
pub fn rt_mode(token: Token, mode: ColorMode) -> ratatui::style::Color {
    use ratatui::style::Color;
    let (r, g, b, c256, c16) = token.spec();
    match mode {
        ColorMode::Truecolor | ColorMode::None => Color::Rgb(r, g, b),
        ColorMode::Ansi256 => Color::Indexed(c256),
        ColorMode::Ansi16 => Color::Indexed(sgr_fg_to_index(c16)),
    }
}

/// Syntax-highlight color for a token kind, finalized via the Claude Design
/// "OrcaRein 终端设计系统" §09 (cold lexicon / warm literals on the #16223C code
/// bg). Tiers explicitly across `mode` so a 256/16-color SBC ssh session gets the
/// design's chosen approximations rather than ratatui's auto-downgrade. NO_COLOR
/// retreats every kind to the body fg (`None`); `Plain` is never colored. Only the
/// fg is overridden — the code-block bg stays.
#[cfg(feature = "tui")]
pub fn syn_color(kind: crate::syntax::SynKind, mode: ColorMode) -> Option<ratatui::style::Color> {
    use crate::syntax::SynKind::*;
    use ratatui::style::Color;
    if mode == ColorMode::None {
        return None; // every syntax color retreats to fg under NO_COLOR
    }
    // (truecolor RGB, 256-color index, 16-color palette index) per design §09.
    let tier = |r: u8, g: u8, b: u8, c256: u8, c16: u8| match mode {
        ColorMode::Truecolor => Color::Rgb(r, g, b),
        ColorMode::Ansi256 => Color::Indexed(c256),
        ColorMode::Ansi16 => Color::Indexed(c16),
        ColorMode::None => unreachable!("handled above"),
    };
    Some(match kind {
        Keyword => tier(0xB7, 0x9B, 0xE6, 140, 13), // lavender violet
        Str => tier(0x8F, 0xD9, 0xB0, 115, 10),     // mint green
        Number => tier(0xE8, 0xA9, 0x74, 179, 11),  // warm peach
        Comment => rt_mode(Token::Dim, mode),       // delegate → stays in sync if Dim retunes
        Plain => return None,
    })
}

/// Status-bar background (`#16223C`) for the modal / pager bars.
#[cfg(feature = "tui")]
pub const STATUS_BG: ratatui::style::Color = ratatui::style::Color::Rgb(22, 34, 60);

/// Whether to paint with real RGB colors. On a 16-color terminal (or NO_COLOR)
/// the modal / pager surfaces fall back to reverse video instead, since exact
/// RGB muddies on a 16-color palette (design: "16 色 / NO_COLOR → 整条反显").
#[cfg(feature = "tui")]
pub fn use_rgb(mode: ColorMode) -> bool {
    matches!(mode, ColorMode::Truecolor | ColorMode::Ansi256)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip SGR escape sequences so we can assert the visible text survives.
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip until (and including) the SGR terminator 'm'.
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

    #[test]
    fn paint_none_is_identity() {
        assert_eq!(paint(ColorMode::None, Token::Brand, "hello"), "hello");
    }

    #[test]
    fn paint_truecolor_wraps_with_rgb_and_reset() {
        let out = paint(ColorMode::Truecolor, Token::Brand, "x");
        assert!(out.starts_with("\x1b[38;2;77;107;254m"), "got {out:?}");
        assert!(out.ends_with("\x1b[0m"));
    }

    #[test]
    fn paint_256_and_16_use_their_codes() {
        assert!(paint(ColorMode::Ansi256, Token::Accent, "x").contains("\x1b[38;5;44m"));
        assert!(paint(ColorMode::Ansi16, Token::Error, "x").contains("\x1b[91m"));
        assert!(paint(ColorMode::Ansi16, Token::Fg, "x").contains("\x1b[37m"));
    }

    #[test]
    fn paint_preserves_visible_text_for_every_mode() {
        for mode in [
            ColorMode::Truecolor,
            ColorMode::Ansi256,
            ColorMode::Ansi16,
            ColorMode::None,
        ] {
            assert_eq!(strip_sgr(&paint(mode, Token::Dim, "中文 abc")), "中文 abc");
        }
    }

    #[test]
    fn reverse_wraps_and_preserves_text() {
        let out = reverse("def");
        assert!(out.starts_with("\x1b[7m"));
        assert!(out.ends_with("\x1b[0m"));
        assert_eq!(strip_sgr(&out), "def");
    }

    #[test]
    fn classify_suppresses_color_when_not_tty_or_no_color() {
        assert_eq!(
            classify(false, false, Some("truecolor"), Some("xterm-256color")),
            ColorMode::None
        );
        assert_eq!(
            classify(true, true, Some("truecolor"), Some("xterm-256color")),
            ColorMode::None
        );
    }

    #[test]
    fn classify_colorterm_truecolor_wins() {
        assert_eq!(
            classify(true, false, Some("truecolor"), Some("xterm")),
            ColorMode::Truecolor
        );
        assert_eq!(
            classify(true, false, Some("24bit"), None),
            ColorMode::Truecolor
        );
    }

    #[cfg(feature = "tui")]
    #[test]
    fn syn_color_tiers_explicitly_by_mode() {
        use crate::syntax::SynKind;
        use ratatui::style::Color;
        // Truecolor → the design's exact RGB (variant-level: hex may retune).
        assert!(matches!(
            syn_color(SynKind::Keyword, ColorMode::Truecolor),
            Some(Color::Rgb(..))
        ));
        // Ansi256 → the design §09 explicit 256-color approximations.
        assert_eq!(
            syn_color(SynKind::Keyword, ColorMode::Ansi256),
            Some(Color::Indexed(140))
        );
        assert_eq!(
            syn_color(SynKind::Str, ColorMode::Ansi256),
            Some(Color::Indexed(115))
        );
        assert_eq!(
            syn_color(SynKind::Number, ColorMode::Ansi256),
            Some(Color::Indexed(179))
        );
        // Ansi16 → the design §09 explicit 16-color palette indices.
        assert_eq!(
            syn_color(SynKind::Keyword, ColorMode::Ansi16),
            Some(Color::Indexed(13))
        );
        assert_eq!(
            syn_color(SynKind::Str, ColorMode::Ansi16),
            Some(Color::Indexed(10))
        );
        assert_eq!(
            syn_color(SynKind::Number, ColorMode::Ansi16),
            Some(Color::Indexed(11))
        );
        // NO_COLOR → every syntax kind retreats to the body fg.
        assert_eq!(syn_color(SynKind::Keyword, ColorMode::None), None);
        assert_eq!(syn_color(SynKind::Str, ColorMode::None), None);
        // Plain identifiers are never colored, in any mode.
        assert_eq!(syn_color(SynKind::Plain, ColorMode::Truecolor), None);
        assert_eq!(syn_color(SynKind::Plain, ColorMode::Ansi16), None);
    }

    #[cfg(feature = "tui")]
    #[test]
    fn syn_color_comment_delegates_to_dim_across_tiers() {
        use crate::syntax::SynKind;
        use ratatui::style::Color;
        // Comment follows Token::Dim, tiering with the mode (so it stays in sync if
        // Dim retunes) — colored in every tier, only NO_COLOR retreats to fg.
        assert_eq!(
            syn_color(SynKind::Comment, ColorMode::Truecolor),
            Some(rt(Token::Dim))
        );
        assert_eq!(
            syn_color(SynKind::Comment, ColorMode::Ansi256),
            Some(rt_mode(Token::Dim, ColorMode::Ansi256))
        );
        assert!(matches!(
            syn_color(SynKind::Comment, ColorMode::Ansi16),
            Some(Color::Indexed(_))
        ));
        assert_eq!(syn_color(SynKind::Comment, ColorMode::None), None);
    }

    #[cfg(feature = "tui")]
    #[test]
    fn rt_mode_maps_sgr_fg_codes_to_palette_indices() {
        use ratatui::style::Color;
        // Token::Dim's 16-color SGR is 90 (bright black) → palette index 8.
        assert_eq!(rt_mode(Token::Dim, ColorMode::Ansi16), Color::Indexed(8));
        // Token::Brand's SGR 94 (bright blue) → index 12; 256 tier passes through.
        assert_eq!(rt_mode(Token::Brand, ColorMode::Ansi16), Color::Indexed(12));
        assert_eq!(
            rt_mode(Token::Brand, ColorMode::Ansi256),
            Color::Indexed(63)
        );
        // Token::Fg's SGR 37 (white) → index 7 (the 30-37 range).
        assert_eq!(rt_mode(Token::Fg, ColorMode::Ansi16), Color::Indexed(7));
    }

    #[test]
    fn classify_falls_back_by_term() {
        // No COLORTERM: decide by TERM.
        assert_eq!(
            classify(true, false, None, Some("xterm-256color")),
            ColorMode::Ansi256
        );
        assert_eq!(
            classify(true, false, None, Some("xterm")),
            ColorMode::Ansi16
        );
        // Unset TERM (Windows / modern terminal) → optimistic truecolor.
        assert_eq!(classify(true, false, None, None), ColorMode::Truecolor);
        // Explicit dumb terminal → no color even on a tty.
        assert_eq!(classify(true, false, None, Some("dumb")), ColorMode::None);
    }
}
