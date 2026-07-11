//! Pixel-orca art ported verbatim from the landing page (`site/app.js` `MAP`),
//! rendered into the terminal with Unicode half-blocks. Pure ANSI + `ColorMode`
//! (no ratatui), so this compiles even under `--no-default-features`. The frame
//! builders are pure and unit-tested; only `swim_once` does terminal I/O.

#[allow(unused_imports)]
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
}
