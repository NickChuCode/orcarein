//! Pixel-orca art ported verbatim from the landing page (`site/app.js` `MAP`),
//! rendered into the terminal with Unicode half-blocks. Pure ANSI + `ColorMode`
//! (no ratatui), so this compiles even under `--no-default-features`. The frame
//! builders are pure and unit-tested; only `swim_once` does terminal I/O.

use crate::color::ColorMode;

/// The hero orca, 16 rows × 35 cols, copied character-for-character from
/// `site/app.js`. `.`=transparent, `K`=body (edge-lit), `W`=belly, `G`=grey
/// detail, `T`=teal eye.
#[allow(dead_code)]
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
    #[allow(dead_code)]
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

/// Nearest xterm-256 color-cube index (cube-only — the design's own 256 tiers
/// were derived this way, e.g. #4D6BFE→63, #2BD4C4→44).
#[allow(dead_code)]
fn to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    fn nearest(v: u8) -> u8 {
        let mut best = 0u8;
        let mut bd = i32::MAX;
        let mut i = 0usize;
        while i < 6 {
            let d = (v as i32 - LEVELS[i] as i32).abs();
            if d < bd {
                bd = d;
                best = i as u8;
            }
            i += 1;
        }
        best
    }
    16 + 36 * nearest(r) + 6 * nearest(g) + nearest(b)
}

/// True when `(y,x)` is outside the map or a transparent `.` cell.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub(crate) const WHALE_W: usize = 36;

/// One character cell packs two vertical pixels (upper=`top`, lower=`bottom`).
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct HalfCell {
    top: Option<WCol>,
    bottom: Option<WCol>,
}

/// Pack a pixel grid into half-block rows: output row `r` combines grid rows
/// `2r` (top) and `2r+1` (bottom). Assumes an even number of rows.
#[allow(dead_code)]
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
#[allow(dead_code)]
fn fg_sgr(c: WCol, mode: ColorMode) -> String {
    let (r, g, b) = c.rgb();
    match mode {
        ColorMode::Truecolor => format!("\x1b[38;2;{r};{g};{b}m"),
        ColorMode::Ansi256 => format!("\x1b[38;5;{}m", to_ansi256(r, g, b)),
        _ => String::new(),
    }
}

/// SGR opener for a background color in `mode`.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
const MINI: &[&str] = &[
    "......KKKKK...",
    ".KKKKKKKKKKKT.",
    ".KKKKKKKKKKKK.",
    "....WWWWWWW...",
];

/// The mini whale packed to 2 half-block rows; `flip` mirrors it horizontally
/// (so it faces left when swimming right→left).
#[allow(dead_code)]
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
#[allow(dead_code)]
pub(crate) const MINI_W: usize = 14;

/// Render the mini whale onto a `cols`-wide field with its left edge at column
/// `x` (may be negative / past the edge — cells off-field are clipped). Returns
/// 2 painted rows; trailing blank cells are trimmed (the animator uses `\x1b[K`
/// to clear leftovers), so each row's visible width is ≤ `cols`.
#[allow(dead_code)]
pub(crate) fn swim_frame(mode: ColorMode, x: i32, cols: usize, flip: bool) -> Vec<String> {
    let blank = HalfCell {
        top: None,
        bottom: None,
    };
    mini_cells(flip)
        .iter()
        .map(|whale_row| {
            let mut field = vec![blank; cols];
            for (i, cell) in whale_row.iter().enumerate() {
                let fx = x + i as i32;
                if fx >= 0 && (fx as usize) < cols {
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
/// least `MINI_W` wide — callers should already gate on `fancy`. Direction is
/// pseudo-random (nanosecond parity); the whale faces the way it swims. This is
/// the only function here that touches the terminal, and it is best-effort:
/// cursor moves are relative and assume the rows it just printed are intact.
pub(crate) async fn swim_once(mode: ColorMode, cols: u16) {
    use std::io::Write;
    if !matches!(mode, ColorMode::Truecolor | ColorMode::Ansi256) || (cols as usize) < MINI_W {
        return;
    }
    const FRAME_MS: u64 = 35;
    const STEP: i32 = 3;
    let flip = nanos() % 2 == 1; // true → swim right→left (faces left)
    let w = MINI_W as i32;
    let cols_i = cols as i32;
    let (mut x, past) = if flip { (cols_i, -w) } else { (-w, cols_i) };

    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b[?25l"); // hide cursor
    let mut first = true;
    loop {
        let done = if flip { x < past } else { x > past };
        if done {
            break;
        }
        if !first {
            let _ = write!(out, "\x1b[2A"); // back to top of the 2-row region
        }
        first = false;
        let rows = swim_frame(mode, x, cols as usize, flip);
        let _ = writeln!(out, "\r\x1b[K{}", rows[0]);
        let _ = writeln!(out, "\r\x1b[K{}", rows[1]);
        let _ = out.flush();
        tokio::time::sleep(std::time::Duration::from_millis(FRAME_MS)).await;
        x += if flip { -STEP } else { STEP };
    }
    // Wipe the region and restore the cursor.
    let _ = write!(out, "\x1b[2A\r\x1b[J\x1b[?25h");
    let _ = out.flush();
}

/// Nanoseconds since the epoch, for a cheap coin flip without adding `rand`.
fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_ansi256_matches_design_tiers() {
        // The design's Brand/Accent 256 tiers must fall out of this cube map.
        assert_eq!(to_ansi256(0x4D, 0x6B, 0xFE), 63); // Brand
        assert_eq!(to_ansi256(0x2B, 0xD4, 0xC4), 44); // Accent / Eye
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
}
