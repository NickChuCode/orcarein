//! Pixel-orca art ported verbatim from the landing page (`site/app.js` `MAP`),
//! rendered into the terminal with Unicode half-blocks — escape codes, not
//! widgets, so nothing here touches `ratatui`. It is still gated behind `tui`:
//! a whale is a terminal-UI ornament, and the lean `--no-default-features`
//! build reports `fancy = false` (see `main::header_env`), so it could never
//! show one anyway. The frame builders are pure and unit-tested; only
//! `swim_once` touches the terminal.

use crate::color::ColorMode;

/// The hero orca, 16 rows × 36 cols, copied character-for-character from
/// `site/app.js`. `.`=transparent, `K`=body (edge-lit), `W`=belly, `G`=grey
/// detail, `T`=teal eye.
pub(crate) const MAP: &[&str] = &[
    "...............KK...................",
    "...............KKK..................",
    "..............KKKK..................",
    "..............KKKKK.................",
    ".............KKKKKKK................",
    ".KKK.....KKKKKKKKKKKKKKKKKK.........",
    ".KKKK..KKKKKKTKKKGGGKKKKKKWWWWK.....",
    "..KKKKKKKKKKKKKKKKKKKKKKKKWWWWWKK...",
    "...KKKKKKKKKKKKKKKKKKKKKKKKWWWKKKKK.",
    "...KKKKKKKKKKKKKKKKKKKKKKKKKKKKKKKKK",
    ".KKK.KKKKKKKKKKKKKKKKKKKKKKWWWWWWWK.",
    ".KK...KKKKKKWWWWWWWKKKKKKKWWWWWWWK..",
    "........KKWWWWWWWWWWWWWWWWWWWWKK....",
    "...........WWWWWWKKKWWWWWWWW........",
    ".................KKK................",
    "..................KK................",
];

/// A resolved concrete color. Values are the design's hexes (see `site/app.js`
/// `COL` + the `K` edge-lighting in `draw`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WCol {
    BodyBase, // #33437A interior
    BodyTop,  // #7A94E8 top edge (lighter)
    BodySide, // #4E5F9E bottom/side edge (darker)
    Belly,    // #EAF0FA
    Grey,     // #8FA3C8
    Eye,      // #2BD4C4 teal
}

impl WCol {
    fn rgb(self) -> (u8, u8, u8) {
        match self {
            WCol::BodyBase => (0x33, 0x43, 0x7A),
            WCol::BodyTop => (0x7A, 0x94, 0xE8),
            WCol::BodySide => (0x4E, 0x5F, 0x9E),
            WCol::Belly => (0xEA, 0xF0, 0xFA),
            WCol::Grey => (0x8F, 0xA3, 0xC8),
            WCol::Eye => (0x2B, 0xD4, 0xC4),
        }
    }
}

/// Nearest xterm-256 color: the 6×6×6 cube *or* the 24-step greyscale ramp,
/// whichever is closer. The ramp is load-bearing — the belly (#EAF0FA) lands on
/// cube index 195, a warm off-white, but on grey 255, which is exactly the tier
/// `color.rs` gives `Token::OrcaWhite` (same RGB). Cube-only would paint the
/// whale's belly and the header's white text as two different whites on one
/// screen. Brand (#4D6BFE→63) and Accent (#2BD4C4→44) still come out of the cube.
fn to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    fn nearest_level(v: i32) -> usize {
        let mut best = 0usize;
        let mut bd = i32::MAX;
        for (i, l) in LEVELS.iter().enumerate() {
            let d = (v - l).abs();
            if d < bd {
                bd = d;
                best = i;
            }
        }
        best
    }
    /// Weighted, not plain Euclidean. Raw RGB distance is a poor perceptual
    /// metric: it puts the body's #33437A on grey 239 over cube 60 by a 3.5%
    /// margin — brightness preserved, brand blue thrown away, whale rendered as
    /// a grey blob with blue edges. Weighting the channels the way the eye does
    /// (green most, red least) keeps every body color in the cube and still
    /// sends the belly to the grey ramp, which is the whole point of having one.
    fn dist(a: (i32, i32, i32), b: (i32, i32, i32)) -> i32 {
        let (dr, dg, db) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
        2 * dr * dr + 4 * dg * dg + 3 * db * db
    }

    let target = (r as i32, g as i32, b as i32);
    let (ri, gi, bi) = (
        nearest_level(target.0),
        nearest_level(target.1),
        nearest_level(target.2),
    );
    let cube = (16 + 36 * ri + 6 * gi + bi) as u8;
    let cube_d = dist(target, (LEVELS[ri], LEVELS[gi], LEVELS[bi]));

    // Greyscale ramp: indices 232..=255 hold the levels 8, 18, … 238.
    let avg = (target.0 + target.1 + target.2) / 3;
    let step = ((avg - 8 + 5) / 10).clamp(0, 23);
    let level = 8 + 10 * step;
    let grey = (232 + step) as u8;
    let grey_d = dist(target, (level, level, level));

    if grey_d < cube_d {
        grey
    } else {
        cube
    }
}

/// True when `(y,x)` is outside the map or a transparent `.` cell.
fn is_empty(map: &[&str], y: i32, x: i32) -> bool {
    if y < 0 || x < 0 || y as usize >= map.len() {
        return true;
    }
    let row = map[y as usize].as_bytes();
    x as usize >= row.len() || row[x as usize] == b'.'
}

/// Parse `map` into a color grid, applying the `K` edge-lighting from the
/// original (based on 16-row coordinates, so it must run before half-block
/// packing).
fn resolve_map(map: &[&str]) -> Vec<Vec<Option<WCol>>> {
    let mut grid = Vec::with_capacity(map.len());
    for (y, line) in map.iter().enumerate() {
        let mut row = Vec::with_capacity(line.len());
        for (x, ch) in line.bytes().enumerate() {
            let c = match ch {
                b'.' => None,
                b'W' => Some(WCol::Belly),
                b'G' => Some(WCol::Grey),
                b'T' => Some(WCol::Eye),
                b'K' => {
                    let (yi, xi) = (y as i32, x as i32);
                    if is_empty(map, yi - 1, xi) {
                        Some(WCol::BodyTop)
                    } else if is_empty(map, yi + 1, xi)
                        || is_empty(map, yi, xi - 1)
                        || is_empty(map, yi, xi + 1)
                    {
                        Some(WCol::BodySide)
                    } else {
                        Some(WCol::BodyBase)
                    }
                }
                _ => None,
            };
            row.push(c);
        }
        grid.push(row);
    }
    grid
}

/// The width of the hero whale in terminal columns (== `MAP` width).
pub(crate) const WHALE_W: usize = 36;

/// One character cell packs two vertical pixels (upper=`top`, lower=`bottom`).
#[derive(Clone, Copy)]
struct HalfCell {
    top: Option<WCol>,
    bottom: Option<WCol>,
}

/// Pack a pixel grid into half-block rows: output row `r` combines grid rows
/// `2r` (top) and `2r+1` (bottom). A trailing odd row is dropped, so callers
/// that index the result must supply an even-height map (`MAP` is 16, `MINI` 4).
fn pack(grid: &[Vec<Option<WCol>>]) -> Vec<Vec<HalfCell>> {
    let mut out = Vec::with_capacity(grid.len() / 2);
    let mut r = 0usize;
    while r + 1 < grid.len() {
        let (top, bot) = (&grid[r], &grid[r + 1]);
        let w = top.len().max(bot.len());
        let mut row = Vec::with_capacity(w);
        for c in 0..w {
            row.push(HalfCell {
                top: top.get(c).copied().flatten(),
                bottom: bot.get(c).copied().flatten(),
            });
        }
        out.push(row);
        r += 2;
    }
    out
}

/// SGR opener for a foreground color in `mode` (`""` if the mode can't paint).
fn fg_sgr(c: WCol, mode: ColorMode) -> String {
    let (r, g, b) = c.rgb();
    match mode {
        ColorMode::Truecolor => format!("\x1b[38;2;{r};{g};{b}m"),
        ColorMode::Ansi256 => format!("\x1b[38;5;{}m", to_ansi256(r, g, b)),
        _ => String::new(),
    }
}

/// SGR opener for a background color in `mode`.
fn bg_sgr(c: WCol, mode: ColorMode) -> String {
    let (r, g, b) = c.rgb();
    match mode {
        ColorMode::Truecolor => format!("\x1b[48;2;{r};{g};{b}m"),
        ColorMode::Ansi256 => format!("\x1b[48;5;{}m", to_ansi256(r, g, b)),
        _ => String::new(),
    }
}

/// Paint one half-block row to an ANSI string. Both pixels present & equal →
/// `█` (fg only); differ → `▀` (fg=top, bg=bottom); one present → `▀`/`▄`;
/// neither → space. Each emitted glyph is width-1, so the row's visible width
/// equals its cell count.
fn paint_half_row(row: &[HalfCell], mode: ColorMode) -> String {
    const RESET: &str = "\x1b[0m";
    let mut out = String::new();
    for cell in row {
        match (cell.top, cell.bottom) {
            (None, None) => out.push(' '),
            (Some(t), None) => {
                out.push_str(&fg_sgr(t, mode));
                out.push('▀');
                out.push_str(RESET);
            }
            (None, Some(b)) => {
                out.push_str(&fg_sgr(b, mode));
                out.push('▄');
                out.push_str(RESET);
            }
            (Some(t), Some(b)) if t == b => {
                out.push_str(&fg_sgr(t, mode));
                out.push('█');
                out.push_str(RESET);
            }
            (Some(t), Some(b)) => {
                out.push_str(&fg_sgr(t, mode));
                out.push_str(&bg_sgr(b, mode));
                out.push('▀');
                out.push_str(RESET);
            }
        }
    }
    out
}

/// The hero whale as painted ANSI lines (8 rows), or empty when the terminal
/// can't do it justice (only Truecolor/Ansi256, and only when `cols` fits the
/// 36-wide art). Honest degradation: everything else gets no banner.
pub(crate) fn whale_banner(mode: ColorMode, cols: u16) -> Vec<String> {
    if !matches!(mode, ColorMode::Truecolor | ColorMode::Ansi256) || (cols as usize) < WHALE_W {
        return Vec::new();
    }
    pack(&resolve_map(MAP))
        .iter()
        .map(|row| paint_half_row(row, mode))
        .collect()
}

/// The tiny swimming orca, 4 rows × 14 cols (head to the right by default).
/// Simpler than the hero whale on purpose — a light accent, not a faithful
/// shrink. `K`/`W`/`T`/`.` as in `MAP`.
const MINI: &[&str] = &[
    "......KKKKK...",
    ".KKKKKKKKKKKT.",
    ".KKKKKKKKKKKK.",
    "....WWWWWWW...",
];

/// The mini whale packed to 2 half-block rows; `flip` mirrors it horizontally
/// (so it faces left when swimming right→left).
fn mini_cells(flip: bool) -> Vec<Vec<HalfCell>> {
    let mut rows = pack(&resolve_map(MINI));
    if flip {
        for row in &mut rows {
            row.reverse();
        }
    }
    rows
}

/// The whale's width in columns (== `MINI` width).
pub(crate) const MINI_W: usize = 14;

/// Render the mini whale onto a field one column narrower than `cols`, with its
/// left edge at column `x` (may be negative / past the edge — cells off-field
/// are clipped). Returns 2 painted rows; trailing blank cells are trimmed (the
/// animator uses `\x1b[K` to clear leftovers).
///
/// The field stops one short of `cols` on purpose. A row that fills the very
/// last cell leaves terminals which wrap eagerly (legacy conhost, minimal
/// emulators, serial consoles — and Linux SBCs over SSH are a runtime target)
/// sitting on the next line already; the following `\n` would then drop a
/// second line, the region would grow to 3–4 rows, and the animator's relative
/// `\x1b[2A` would start eating whatever is above it.
pub(crate) fn swim_frame(mode: ColorMode, x: i32, cols: usize, flip: bool) -> Vec<String> {
    let usable = cols.saturating_sub(1);
    let blank = HalfCell {
        top: None,
        bottom: None,
    };
    mini_cells(flip)
        .iter()
        .map(|whale_row| {
            let mut field = vec![blank; usable];
            for (i, cell) in whale_row.iter().enumerate() {
                let fx = x + i as i32;
                if fx >= 0 && (fx as usize) < usable {
                    field[fx as usize] = *cell;
                }
            }
            // Trim trailing blank cells to keep frames short.
            let end = field
                .iter()
                .rposition(|c| c.top.is_some() || c.bottom.is_some())
                .map(|p| p + 1)
                .unwrap_or(0);
            paint_half_row(&field[..end], mode)
        })
        .collect()
}

/// Play the mini whale swimming across the terminal once, then leave the region
/// clean. No-op unless the terminal can paint it (Truecolor/Ansi256) and is at
/// least `MINI_W` wide — callers should already gate on `fancy`. The whale faces
/// the way it swims. This is the only function here that touches the terminal,
/// and it is best-effort: cursor moves are relative and assume the two rows it
/// just printed are still intact.
///
/// **The cursor is deliberately left visible.** Hiding it (`\x1b[?25l`) would
/// need a matching `\x1b[?25h` on *every* exit path, and a Ctrl+C during the
/// animation kills the process before any destructor runs — the user's shell
/// would come back with no cursor, permanently, until `reset`. Catching SIGINT
/// to prevent that is the worse trade: tokio installs its handler for the life
/// of the process and never restores the default, so Ctrl+C would stop killing
/// OrcaRein at all (including mid-response). A cursor riding along with the
/// whale for a second is the cheaper price.
pub(crate) async fn swim_once(mode: ColorMode, cols: u16) {
    use std::io::Write;
    if !matches!(mode, ColorMode::Truecolor | ColorMode::Ansi256) || (cols as usize) < MINI_W {
        return;
    }
    const FRAME_MS: u64 = 35;
    /// Frames per swim, whatever the width. Without this the step is fixed and a
    /// 213-column monitor pays 2.7 s of startup; the step now scales instead, so
    /// the swim always lands around `MAX_FRAMES * FRAME_MS` ≈ 1.1 s.
    const MAX_FRAMES: i32 = 32;

    let flip = coin_flip(); // true → swim right→left (faces left)
    let w = MINI_W as i32;
    let cols_i = cols as i32;
    let step = ((cols_i + w) / MAX_FRAMES).max(3);
    let (mut x, past) = if flip { (cols_i, -w) } else { (-w, cols_i) };

    let mut first = true;
    loop {
        let done = if flip { x < past } else { x > past };
        if done {
            break;
        }
        let rows = swim_frame(mode, x, cols as usize, flip);
        {
            // Re-locked per frame: holding the lock across the `.await` would
            // stall any other task that prints for the whole animation.
            let mut out = std::io::stdout().lock();
            if !first {
                let _ = write!(out, "\x1b[2A"); // back to the top of the 2-row region
            }
            for row in &rows {
                let _ = writeln!(out, "\r\x1b[K{row}");
            }
            let _ = out.flush();
        }
        first = false;
        tokio::time::sleep(std::time::Duration::from_millis(FRAME_MS)).await;
        x += if flip { -step } else { step };
    }

    if !first {
        // Wipe the region. Guarded: with no frame printed there is nothing above
        // us to erase, and `\x1b[J` would eat someone else's output.
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "\x1b[2A\r\x1b[J");
        let _ = out.flush();
    }
}

/// A coin flip without adding `rand`: `RandomState` is seeded from the OS.
///
/// The obvious `SystemTime::now().as_nanos() % 2` is **always even on Windows**,
/// where the clock ticks in 100 ns units — the whale would swim left→right every
/// single time on the platform this is developed on, and no test would notice.
fn coin_flip() -> bool {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
        % 2
        == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_ansi256_matches_design_tiers() {
        // The design's Brand/Accent 256 tiers must fall out of the cube.
        assert_eq!(to_ansi256(0x4D, 0x6B, 0xFE), 63); // Brand
        assert_eq!(to_ansi256(0x2B, 0xD4, 0xC4), 44); // Accent / Eye
    }

    #[test]
    fn belly_takes_the_grey_ramp_and_matches_the_header_white() {
        // Load-bearing: `color::Token::OrcaWhite` has the same RGB and the 256
        // tier 255. Cube-only would give the belly 195 — a second, warmer white
        // sitting right next to the header's text.
        let (r, g, b) = WCol::Belly.rgb();
        assert_eq!((r, g, b), (234, 240, 250));
        assert_eq!(to_ansi256(r, g, b), 255);
    }

    #[test]
    fn ansi256_tiers_for_every_whale_color() {
        // Pin all six, not just the interesting ones. An earlier cut asserted
        // only Brand/Accent/Belly and shipped a `to_ansi256` that quietly sent
        // BodyBase — the whale's entire torso — to grey 239.
        for (c, want) in [
            (WCol::BodyBase, 60),
            (WCol::BodyTop, 104),
            (WCol::BodySide, 61),
            (WCol::Belly, 255),
            (WCol::Grey, 110),
            (WCol::Eye, 44),
        ] {
            let (r, g, b) = c.rgb();
            assert_eq!(to_ansi256(r, g, b), want, "{c:?}");
        }
    }

    #[test]
    fn map_is_16x36() {
        assert_eq!(MAP.len(), 16);
        for l in MAP {
            assert_eq!(l.len(), 36, "map row not 36 wide: {l:?}");
        }
    }

    #[test]
    fn edge_lighting_picks_top_side_interior() {
        // A solid 3×3 K block: center is interior, top-middle is top edge,
        // middle-left is a side edge.
        let block = &["KKK", "KKK", "KKK"][..];
        let g = resolve_map(block);
        assert_eq!(g[1][1], Some(WCol::BodyBase), "center is interior");
        assert_eq!(g[0][1], Some(WCol::BodyTop), "top row lit");
        assert_eq!(g[1][0], Some(WCol::BodySide), "left edge darker");
    }

    #[test]
    fn resolve_maps_glyphs_to_colors() {
        let g = resolve_map(&["W.TG"][..]);
        assert_eq!(g[0][0], Some(WCol::Belly));
        assert_eq!(g[0][1], None);
        assert_eq!(g[0][2], Some(WCol::Eye));
        assert_eq!(g[0][3], Some(WCol::Grey));
    }

    /// Remove SGR escapes so we can measure visible width.
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

    #[test]
    fn banner_truecolor_is_8_rows_each_width_36() {
        use unicode_width::UnicodeWidthStr;
        let lines = whale_banner(ColorMode::Truecolor, 100);
        assert_eq!(lines.len(), 8);
        for l in &lines {
            assert_eq!(UnicodeWidthStr::width(strip_sgr(l).as_str()), 36);
        }
    }

    #[test]
    fn banner_empty_under_none_or_narrow() {
        assert!(whale_banner(ColorMode::None, 100).is_empty());
        assert!(whale_banner(ColorMode::Ansi16, 100).is_empty());
        assert!(whale_banner(ColorMode::Truecolor, 20).is_empty());
    }

    #[test]
    fn paint_row_glyph_selection() {
        let row = vec![
            HalfCell {
                top: None,
                bottom: None,
            },
            HalfCell {
                top: Some(WCol::Belly),
                bottom: None,
            },
            HalfCell {
                top: None,
                bottom: Some(WCol::Belly),
            },
            HalfCell {
                top: Some(WCol::Belly),
                bottom: Some(WCol::Belly),
            },
            HalfCell {
                top: Some(WCol::Eye),
                bottom: Some(WCol::Belly),
            },
        ];
        let painted = strip_sgr(&paint_half_row(&row, ColorMode::Truecolor));
        assert_eq!(painted, " ▀▄█▀");
    }

    #[test]
    fn mini_is_4x14() {
        assert_eq!(MINI.len(), 4);
        for l in MINI {
            assert_eq!(l.len(), 14);
        }
    }

    #[test]
    fn swim_frame_offset_shifts_right() {
        let near = strip_sgr(&swim_frame(ColorMode::Truecolor, 0, 60, false)[1]);
        let far = strip_sgr(&swim_frame(ColorMode::Truecolor, 20, 60, false)[1]);
        let lead = |s: &str| s.chars().take_while(|c| *c == ' ').count();
        assert!(lead(&far) > lead(&near), "larger x → more leading spaces");
    }

    #[test]
    fn swim_frame_fully_offscreen_is_blank() {
        // Far to the left: nothing visible, no color escapes.
        let rows = swim_frame(ColorMode::Truecolor, -100, 40, false);
        for r in rows {
            assert!(!r.contains('\x1b'), "no color when off-field");
            assert!(r.trim().is_empty());
        }
    }

    #[test]
    fn swim_frame_flip_differs() {
        let a = swim_frame(ColorMode::Truecolor, 5, 60, false);
        let b = swim_frame(ColorMode::Truecolor, 5, 60, true);
        assert_ne!(a, b, "flip mirrors the whale");
    }

    #[test]
    fn swim_frame_never_fills_the_last_column() {
        // A row exactly `cols` wide leaves eager-wrapping terminals on the next
        // line, which desyncs the animator's relative `\x1b[2A`. Sweep the whale
        // clean across the field, in both directions, and check every frame.
        use unicode_width::UnicodeWidthStr;
        for cols in [14usize, 40, 80, 213] {
            for flip in [false, true] {
                for x in -20i32..=(cols as i32 + 20) {
                    for row in swim_frame(ColorMode::Truecolor, x, cols, flip) {
                        let w = UnicodeWidthStr::width(strip_sgr(&row).as_str());
                        assert!(
                            w < cols,
                            "frame at x={x} cols={cols} flip={flip} is {w} wide"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn banner_ansi256_keeps_width() {
        // The 256 path builds different SGR openers than truecolor; the visible
        // width must survive them too.
        use unicode_width::UnicodeWidthStr;
        let lines = whale_banner(ColorMode::Ansi256, 100);
        assert_eq!(lines.len(), 8);
        for l in &lines {
            assert_eq!(UnicodeWidthStr::width(strip_sgr(l).as_str()), 36);
        }
    }

    #[test]
    fn coin_flip_yields_both_directions() {
        // Regression: the old `SystemTime::now().as_nanos() % 2` is always even
        // on Windows (100 ns clock), so the whale only ever swam one way there.
        let mut seen = (false, false);
        for _ in 0..128 {
            if coin_flip() {
                seen.0 = true;
            } else {
                seen.1 = true;
            }
        }
        assert_eq!(seen, (true, true), "coin flip is stuck on one face");
    }
}
