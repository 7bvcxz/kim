//! Glyphs for the cell-grid renderer: the [`Rasterizer`] boundary (system fonts through GDI on
//! Windows, a deterministic built-in rasterizer for tests and other platforms), box-drawing and
//! block characters drawn procedurally so lines join across cells at any line height, and the
//! font `cmap` lookup that decides which face of a fallback chain draws a character.

use std::collections::HashMap;

/// Cell geometry in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// Baseline, from the cell's top.
    pub baseline: u32,
    /// Top of the underline, from the cell's top.
    pub underline: u32,
    /// Top of the strike-through line, from the cell's top.
    pub strike: u32,
    /// Thickness of decoration lines and light box-drawing strokes.
    pub stroke: u32,
}

/// Bold and italic, the styles a glyph is rasterized in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    /// 0 regular, 1 bold, 2 italic, 3 bold italic.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn index(self) -> usize {
        usize::from(self.bold) + 2 * usize::from(self.italic)
    }
}

/// Which face draws a character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceInfo {
    /// The family name, `procedural` for box drawing, or empty when no face has the character.
    pub family: String,
    /// Not the first face of the chain.
    pub fallback: bool,
    pub found: bool,
}

/// A glyph's coverage: one byte per pixel (0 = background, 255 = foreground), positioned
/// relative to the top-left corner of the cells it spans. It may reach outside them (italic
/// overhang); the renderer clips it to the row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlyphBitmap {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

impl GlyphBitmap {
    fn blank(width: u32, height: u32) -> Self {
        Self {
            left: 0,
            top: 0,
            width,
            height,
            coverage: vec![0; (width * height) as usize],
        }
    }

    fn fill(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, v: u8) {
        let (w, h) = (i64::from(self.width), i64::from(self.height));
        for y in y0.max(0)..y1.min(h) {
            for x in x0.max(0)..x1.min(w) {
                let i = (y * w + x) as usize;
                self.coverage[i] = self.coverage[i].max(v);
            }
        }
    }

    /// Trims empty borders (keeps memory per cached glyph small).
    pub(crate) fn trimmed(mut self) -> Self {
        let (w, h) = (self.width as usize, self.height as usize);
        let ink = |x: usize, y: usize| self.coverage[y * w + x] != 0;
        let rows: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| ink(x, y))).collect();
        let (Some(&y0), Some(&y1)) = (rows.first(), rows.last()) else {
            return Self {
                left: self.left,
                top: self.top,
                ..Self::default()
            };
        };
        let cols: Vec<usize> = (0..w).filter(|&x| (y0..=y1).any(|y| ink(x, y))).collect();
        let (x0, x1) = (cols[0], cols[cols.len() - 1]);
        let mut cov = Vec::with_capacity((x1 - x0 + 1) * (y1 - y0 + 1));
        for y in y0..=y1 {
            cov.extend_from_slice(&self.coverage[y * w + x0..=y * w + x1]);
        }
        self.left += x0 as i32;
        self.top += y0 as i32;
        self.width = (x1 - x0 + 1) as u32;
        self.height = (y1 - y0 + 1) as u32;
        self.coverage = cov;
        self
    }

    /// Bytes this glyph holds.
    pub fn bytes(&self) -> usize {
        self.coverage.capacity() + size_of::<Self>()
    }

    /// Pixels with any ink.
    pub fn ink(&self) -> usize {
        self.coverage.iter().filter(|&&c| c != 0).count()
    }
}

/// Draws glyphs. Implementations own their font resources; the renderer caches what they draw.
pub trait Rasterizer: std::fmt::Debug {
    /// A short name for evidence (`gdi`, `builtin`).
    fn name(&self) -> &str;

    /// Sets the font's em size and the wanted cell height in device pixels; returns the cell
    /// metrics (the height may grow if the font needs more).
    fn configure(&mut self, em_px: u32, cell_height: u32) -> CellMetrics;

    /// Draws `cluster` (a character and its combining marks) across `cells` columns.
    fn rasterize(&mut self, cluster: &str, style: Style, cells: u8, m: &CellMetrics)
    -> GlyphBitmap;

    /// Which face draws `ch`.
    fn face_for(&mut self, ch: char) -> FaceInfo;
}

// ---------------------------------------------------------------------------------------------
// The built-in rasterizer.

/// A deterministic rasterizer that needs no font: every character is a box whose inside shows
/// the low bits of its code point as dots. Pixel-exact on every machine, so golden frames of the
/// renderer (layout, colours, wide characters, decorations, cursor, selection) are stable; it is
/// also what other platforms draw with until a system-font rasterizer exists for them.
#[derive(Debug, Default)]
pub struct BoxRasterizer {
    em: u32,
}

impl BoxRasterizer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Rasterizer for BoxRasterizer {
    fn name(&self) -> &str {
        "builtin"
    }

    fn configure(&mut self, em_px: u32, cell_height: u32) -> CellMetrics {
        self.em = em_px.max(4);
        let width = (self.em * 3).div_ceil(5).max(3);
        let height = cell_height.max(self.em + 2);
        let stroke = (self.em / 12).max(1);
        let baseline = height - height / 5;
        CellMetrics {
            width,
            height,
            baseline,
            underline: (baseline + stroke).min(height - stroke),
            strike: height / 2,
            stroke,
        }
    }

    fn rasterize(
        &mut self,
        cluster: &str,
        style: Style,
        cells: u8,
        m: &CellMetrics,
    ) -> GlyphBitmap {
        let w = m.width * u32::from(cells);
        let mut g = GlyphBitmap::blank(w, m.height);
        let t = i64::from(if style.bold { m.stroke + 1 } else { m.stroke });
        let (x0, x1) = (1, i64::from(w) - 1);
        let (y0, y1) = (i64::from(m.height / 6), i64::from(m.baseline));
        // The outline.
        g.fill(x0, y0, x1, y0 + t, 255);
        g.fill(x0, y1 - t, x1, y1, 255);
        g.fill(x0, y0, x0 + t, y1, 255);
        g.fill(x1 - t, y0, x1, y1, 255);
        // The code point's low 9 bits as a 3×3 grid of dots (combining marks change it).
        let code = cluster.chars().fold(0u32, |acc, c| {
            acc.wrapping_mul(31).wrapping_add(u32::from(c))
        });
        let (iw, ih) = ((x1 - x0 - 2 * t).max(3), (y1 - y0 - 2 * t).max(3));
        for bit in 0..9 {
            if code >> bit & 1 == 1 {
                let (cx, cy) = (i64::from(bit % 3), i64::from(bit / 3));
                let dx = x0 + t + cx * iw / 3;
                let dy = y0 + t + cy * ih / 3;
                g.fill(dx, dy, dx + (iw / 3).max(1), dy + (ih / 3).max(1), 160);
            }
        }
        if style.italic {
            // Slant: the upper half moves one pixel right.
            let wu = w as usize;
            for y in 0..(m.height as usize / 2) {
                let row = &mut g.coverage[y * wu..(y + 1) * wu];
                row.rotate_right(1);
                row[0] = 0;
            }
        }
        g.trimmed()
    }

    fn face_for(&mut self, _ch: char) -> FaceInfo {
        FaceInfo {
            family: "builtin".into(),
            fallback: false,
            found: true,
        }
    }
}

/// The glyph drawn when no face of the chain has a character: a hollow box (honest "missing",
/// never a wrong character).
pub fn missing_glyph(cells: u8, m: &CellMetrics) -> GlyphBitmap {
    let w = m.width * u32::from(cells);
    let mut g = GlyphBitmap::blank(w, m.height);
    let s = i64::from(m.stroke);
    let (x0, x1) = (
        i64::from(m.width / 6).max(1),
        i64::from(w - m.width / 6).max(2),
    );
    let (y0, y1) = (i64::from(m.height / 5), i64::from(m.baseline));
    g.fill(x0, y0, x1, y0 + s, 255);
    g.fill(x0, y1 - s, x1, y1, 255);
    g.fill(x0, y0, x0 + s, y1, 255);
    g.fill(x1 - s, y0, x1, y1, 255);
    g.trimmed()
}

// ---------------------------------------------------------------------------------------------
// Box drawing and block elements, drawn procedurally.

/// Line weights of one arm of a box-drawing character.
const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// The arms (up, right, down, left) of U+2500–U+257F, or `None` for characters left to the font
/// (dashes and diagonals).
fn box_arms(c: char) -> Option<[u8; 4]> {
    const L: u8 = LIGHT;
    const H: u8 = HEAVY;
    const D: u8 = DOUBLE;
    const N: u8 = NONE;
    const TABLE: [[u8; 4]; 128] = [
        [N, L, N, L],
        [N, H, N, H],
        [L, N, L, N],
        [H, N, H, N], // 2500 ─ ━ │ ┃
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4], // 2504–250B dashes
        [N, L, L, N],
        [N, H, L, N],
        [N, L, H, N],
        [N, H, H, N], // 250C ┌ ┍ ┎ ┏
        [N, N, L, L],
        [N, N, L, H],
        [N, N, H, L],
        [N, N, H, H], // 2510 ┐ ┑ ┒ ┓
        [L, L, N, N],
        [L, H, N, N],
        [H, L, N, N],
        [H, H, N, N], // 2514 └ ┕ ┖ ┗
        [L, N, N, L],
        [L, N, N, H],
        [H, N, N, L],
        [H, N, N, H], // 2518 ┘ ┙ ┚ ┛
        [L, L, L, N],
        [L, H, L, N],
        [H, L, L, N],
        [L, L, H, N], // 251C ├ ┝ ┞ ┟
        [H, L, H, N],
        [H, H, L, N],
        [L, H, H, N],
        [H, H, H, N], // 2520 ┠ ┡ ┢ ┣
        [L, N, L, L],
        [L, N, L, H],
        [H, N, L, L],
        [L, N, H, L], // 2524 ┤ ┥ ┦ ┧
        [H, N, H, L],
        [H, N, L, H],
        [L, N, H, H],
        [H, N, H, H], // 2528 ┨ ┩ ┪ ┫
        [N, L, L, L],
        [N, L, L, H],
        [N, H, L, L],
        [N, H, L, H], // 252C ┬ ┭ ┮ ┯
        [N, L, H, L],
        [N, L, H, H],
        [N, H, H, L],
        [N, H, H, H], // 2530 ┰ ┱ ┲ ┳
        [L, L, N, L],
        [L, L, N, H],
        [L, H, N, L],
        [L, H, N, H], // 2534 ┴ ┵ ┶ ┷
        [H, L, N, L],
        [H, L, N, H],
        [H, H, N, L],
        [H, H, N, H], // 2538 ┸ ┹ ┺ ┻
        [L, L, L, L],
        [L, L, L, H],
        [L, H, L, L],
        [L, H, L, H], // 253C ┼ ┽ ┾ ┿
        [H, L, L, L],
        [L, L, H, L],
        [H, L, H, L],
        [H, L, L, H], // 2540 ╀ ╁ ╂ ╃
        [H, H, L, L],
        [L, L, H, H],
        [L, H, H, L],
        [H, H, L, H], // 2544 ╄ ╅ ╆ ╇
        [L, H, H, H],
        [H, L, H, H],
        [H, H, H, L],
        [H, H, H, H], // 2548 ╈ ╉ ╊ ╋
        [0; 4],
        [0; 4],
        [0; 4],
        [0; 4], // 254C–254F dashes
        [N, D, N, D],
        [D, N, D, N],
        [N, D, L, N],
        [N, L, D, N], // 2550 ═ ║ ╒ ╓
        [N, D, D, N],
        [N, N, L, D],
        [N, N, D, L],
        [N, N, D, D], // 2554 ╔ ╕ ╖ ╗
        [L, D, N, N],
        [D, L, N, N],
        [D, D, N, N],
        [L, N, N, D], // 2558 ╘ ╙ ╚ ╛
        [D, N, N, L],
        [D, N, N, D],
        [L, D, L, N],
        [D, L, D, N], // 255C ╜ ╝ ╞ ╟
        [D, D, D, N],
        [L, N, L, D],
        [D, N, D, L],
        [D, N, D, D], // 2560 ╠ ╡ ╢ ╣
        [N, D, L, D],
        [N, L, D, L],
        [N, D, D, D],
        [L, D, N, D], // 2564 ╤ ╥ ╦ ╧
        [D, L, N, L],
        [D, D, N, D],
        [L, D, L, D],
        [D, L, D, L], // 2568 ╨ ╩ ╪ ╫
        [D, D, D, D],
        [N, L, L, N],
        [N, N, L, L],
        [L, N, N, L], // 256C ╬ ╭ ╮ ╯
        [L, L, N, N],
        [0; 4],
        [0; 4],
        [0; 4], // 2570 ╰, 2571–2573 diagonals
        [N, N, N, L],
        [L, N, N, N],
        [N, L, N, N],
        [N, N, L, N], // 2574 ╴ ╵ ╶ ╷
        [N, N, N, H],
        [H, N, N, N],
        [N, H, N, N],
        [N, N, H, N], // 2578 ╸ ╹ ╺ ╻
        [N, H, N, L],
        [L, N, H, N],
        [N, L, N, H],
        [H, N, L, N], // 257C ╼ ╽ ╾ ╿
    ];
    let n = u32::from(c).checked_sub(0x2500)?;
    let arms = *TABLE.get(n as usize)?;
    (arms != [0; 4]).then_some(arms)
}

/// Box-drawing (U+2500–U+257F, except dashes and diagonals) and block elements (U+2580–U+259F)
/// as exact cell-filling shapes, or `None` for any other character.
pub fn procedural_glyph(c: char, cells: u8, m: &CellMetrics) -> Option<GlyphBitmap> {
    let n = u32::from(c);
    if !(0x2500..=0x259F).contains(&n) {
        return None;
    }
    let w = m.width * u32::from(cells);
    let h = m.height;
    let mut g = GlyphBitmap::blank(w, h);
    let (wi, hi) = (i64::from(w), i64::from(h));
    if n >= 0x2580 {
        // Block elements.
        let frac_h = |e: i64| hi * e / 8;
        let frac_w = |e: i64| wi * e / 8;
        let (hx, hy) = (wi / 2, hi / 2);
        match n {
            0x2580 => g.fill(0, 0, wi, hy, 255),
            0x2581..=0x2588 => {
                let e = i64::from(n - 0x2580);
                g.fill(0, hi - frac_h(e), wi, hi, 255);
            }
            0x2589..=0x258F => {
                let e = 8 - i64::from(n - 0x2588);
                g.fill(0, 0, frac_w(e), hi, 255);
            }
            0x2590 => g.fill(hx, 0, wi, hi, 255),
            0x2591 => g.fill(0, 0, wi, hi, 64),
            0x2592 => g.fill(0, 0, wi, hi, 128),
            0x2593 => g.fill(0, 0, wi, hi, 192),
            0x2594 => g.fill(0, 0, wi, frac_h(1), 255),
            0x2595 => g.fill(wi - frac_w(1), 0, wi, hi, 255),
            _ => {
                // Quadrants: upper-left, upper-right, lower-left, lower-right.
                let q: [bool; 4] = match n {
                    0x2596 => [false, false, true, false],
                    0x2597 => [false, false, false, true],
                    0x2598 => [true, false, false, false],
                    0x2599 => [true, false, true, true],
                    0x259A => [true, false, false, true],
                    0x259B => [true, true, true, false],
                    0x259C => [true, true, false, true],
                    0x259D => [false, true, false, false],
                    0x259E => [false, true, true, false],
                    _ => [false, true, true, true],
                };
                let quads = [
                    (0, 0, hx, hy),
                    (hx, 0, wi, hy),
                    (0, hy, hx, hi),
                    (hx, hy, wi, hi),
                ];
                for (on, (x0, y0, x1, y1)) in q.iter().zip(quads) {
                    if *on {
                        g.fill(x0, y0, x1, y1, 255);
                    }
                }
            }
        }
        return Some(g);
    }
    let arms = box_arms(c)?;
    let light = i64::from(m.stroke.max(1));
    let heavy = light * 2;
    let gap = light;
    let (cx, cy) = (wi / 2, hi / 2);
    let thick = |w: u8| if w == HEAVY { heavy } else { light };
    // Where the line(s) of an arm sit across its direction, relative to the cell's middle.
    let offsets = |w: u8| -> Vec<i64> {
        if w == DOUBLE {
            vec![-(gap + light), gap]
        } else {
            vec![-(thick(w) / 2)]
        }
    };
    // The span the crossing line(s) cover, so arms meet them without a gap or an overshoot.
    let extent = |w: u8| -> (i64, i64) {
        if w == NONE {
            (0, 0)
        } else {
            let o = offsets(w);
            (o[0], o[o.len() - 1] + thick(w))
        }
    };
    let (vlo, vhi) = extent(arms[0].max(arms[2]));
    let (hlo, hhi) = extent(arms[1].max(arms[3]));
    for (i, &weight) in arms.iter().enumerate() {
        if weight == NONE {
            continue;
        }
        let t = thick(weight);
        for off in offsets(weight) {
            match i {
                0 => g.fill(cx + off, 0, cx + off + t, cy + hhi, 255),
                2 => g.fill(cx + off, cy + hlo, cx + off + t, hi, 255),
                1 => g.fill(cx + vlo, cy + off, wi, cy + off + t, 255),
                _ => g.fill(0, cy + off, cx + vhi, cy + off + t, 255),
            }
        }
    }
    Some(g)
}

// ---------------------------------------------------------------------------------------------
// Font cmap lookup.

/// A font's character-to-glyph map (the OpenType `cmap` table), read from its raw bytes: format
/// 12 (all of Unicode) when the font has it, else format 4 (the BMP).
#[derive(Debug, Clone)]
pub struct Cmap {
    data: Vec<u8>,
    /// Offset of the chosen subtable and its format.
    sub: Option<(usize, u16)>,
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(at)?,
        *d.get(at + 1)?,
        *d.get(at + 2)?,
        *d.get(at + 3)?,
    ]))
}

impl Cmap {
    /// Parses a `cmap` table; a table without a usable Unicode subtable maps nothing.
    pub fn parse(data: Vec<u8>) -> Self {
        let mut best: Option<(u8, usize, u16)> = None;
        let count = be16(&data, 2).unwrap_or(0);
        for i in 0..usize::from(count) {
            let rec = 4 + i * 8;
            let (Some(platform), Some(encoding), Some(off)) =
                (be16(&data, rec), be16(&data, rec + 2), be32(&data, rec + 4))
            else {
                break;
            };
            let off = off as usize;
            let Some(format) = be16(&data, off) else {
                continue;
            };
            let rank = match (platform, encoding, format) {
                (3, 10, 12) => 4,
                (0, 4 | 6, 12) => 3,
                (3, 1, 4) => 2,
                (0, _, 4) => 1,
                _ => 0,
            };
            if rank > 0 && best.is_none_or(|(r, _, _)| rank > r) {
                best = Some((rank, off, format));
            }
        }
        Self {
            sub: best.map(|(_, off, f)| (off, f)),
            data,
        }
    }

    /// The glyph index of `c`, 0 when the font does not have it.
    pub fn glyph(&self, c: char) -> u16 {
        let Some((off, format)) = self.sub else {
            return 0;
        };
        let c = u32::from(c);
        let d = &self.data;
        match format {
            12 => {
                let n = be32(d, off + 12).unwrap_or(0) as usize;
                let (mut lo, mut hi) = (0usize, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let g = off + 16 + mid * 12;
                    let (Some(start), Some(end), Some(gid)) =
                        (be32(d, g), be32(d, g + 4), be32(d, g + 8))
                    else {
                        return 0;
                    };
                    if c < start {
                        hi = mid;
                    } else if c > end {
                        lo = mid + 1;
                    } else {
                        return u16::try_from(gid + (c - start)).unwrap_or(0);
                    }
                }
                0
            }
            4 => {
                let Ok(c) = u16::try_from(c) else {
                    return 0;
                };
                let segs = usize::from(be16(d, off + 6).unwrap_or(0) / 2);
                let ends = off + 14;
                let starts = ends + segs * 2 + 2;
                let deltas = starts + segs * 2;
                let ranges = deltas + segs * 2;
                // First segment whose end is at or after c.
                let (mut lo, mut hi) = (0usize, segs);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if be16(d, ends + mid * 2).unwrap_or(0) < c {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                if lo >= segs {
                    return 0;
                }
                let i = lo;
                let start = be16(d, starts + i * 2).unwrap_or(u16::MAX);
                if c < start {
                    return 0;
                }
                let delta = be16(d, deltas + i * 2).unwrap_or(0);
                let ro_at = ranges + i * 2;
                let ro = be16(d, ro_at).unwrap_or(0);
                if ro == 0 {
                    return c.wrapping_add(delta);
                }
                let at = ro_at + usize::from(ro) + usize::from(c - start) * 2;
                match be16(d, at) {
                    Some(0) | None => 0,
                    Some(g) => g.wrapping_add(delta),
                }
            }
            _ => 0,
        }
    }
}

/// The family names (name IDs 1 and 16, every language) in a font's OpenType `name` table.
pub fn font_names(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let (Some(count), Some(storage)) = (be16(data, 2), be16(data, 4)) else {
        return out;
    };
    for i in 0..usize::from(count) {
        let r = 6 + i * 12;
        let (Some(platform), Some(name_id), Some(len), Some(off)) = (
            be16(data, r),
            be16(data, r + 6),
            be16(data, r + 8),
            be16(data, r + 10),
        ) else {
            break;
        };
        if name_id != 1 && name_id != 16 {
            continue;
        }
        let start = usize::from(storage) + usize::from(off);
        let Some(bytes) = data.get(start..start + usize::from(len)) else {
            continue;
        };
        let name = match platform {
            0 | 3 => {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => bytes.iter().map(|&b| char::from(b)).collect(),
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The glyph cache.

/// Glyphs drawn once and reused, keyed by cluster, style and width. Bounded: past
/// [`GlyphCache::LIMIT`] bytes it starts again (a theme or size change also clears it).
#[derive(Debug, Default)]
pub struct GlyphCache {
    single: HashMap<(char, Style, u8), GlyphBitmap>,
    clusters: HashMap<(String, Style, u8), GlyphBitmap>,
    bytes: usize,
    pub hits: u64,
    pub misses: u64,
}

impl GlyphCache {
    /// Bytes of glyph coverage kept at most.
    pub const LIMIT: usize = 8 * 1024 * 1024;

    pub fn clear(&mut self) {
        self.single.clear();
        self.clusters.clear();
        self.bytes = 0;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.single.len() + self.clusters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The glyph of `ch` + `combining` in `style` across `cells`, drawing it on first use.
    pub fn get(
        &mut self,
        raster: &mut dyn Rasterizer,
        m: &CellMetrics,
        ch: char,
        combining: &str,
        style: Style,
        cells: u8,
    ) -> &GlyphBitmap {
        if self.bytes > Self::LIMIT {
            self.clear();
        }
        let draw = |raster: &mut dyn Rasterizer, cluster: &str| {
            let c = cluster.chars().next().unwrap_or(' ');
            procedural_glyph(c, cells, m)
                .unwrap_or_else(|| raster.rasterize(cluster, style, cells, m))
        };
        if combining.is_empty() {
            let key = (ch, style, cells);
            if self.single.contains_key(&key) {
                self.hits += 1;
            } else {
                self.misses += 1;
                let mut buf = [0u8; 4];
                let g = draw(raster, ch.encode_utf8(&mut buf));
                self.bytes += g.bytes();
                self.single.insert(key, g);
            }
            &self.single[&key]
        } else {
            let mut cluster = String::with_capacity(4 + combining.len());
            cluster.push(ch);
            cluster.push_str(combining);
            let key = (cluster, style, cells);
            if self.clusters.contains_key(&key) {
                self.hits += 1;
            } else {
                self.misses += 1;
                let g = draw(raster, &key.0);
                self.bytes += g.bytes();
                self.clusters.insert(key.clone(), g);
            }
            &self.clusters[&key]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        BoxRasterizer::new().configure(13, 16)
    }

    #[test]
    fn builtin_glyphs_are_deterministic_and_distinct() {
        let m = metrics();
        let mut r = BoxRasterizer::new();
        r.configure(13, 16);
        let a = r.rasterize("A", Style::default(), 1, &m);
        assert_eq!(a, r.rasterize("A", Style::default(), 1, &m));
        assert_ne!(a, r.rasterize("B", Style::default(), 1, &m));
        assert_ne!(
            a,
            r.rasterize(
                "A",
                Style {
                    bold: true,
                    italic: false
                },
                1,
                &m
            )
        );
        let wide = r.rasterize("한", Style::default(), 2, &m);
        assert!(wide.width > m.width, "a wide glyph spans two cells");
        assert!(wide.left >= 0 && wide.left + wide.width as i32 <= 2 * m.width as i32);
    }

    #[test]
    fn box_drawing_fills_its_cell_edge_to_edge() {
        let m = metrics();
        let h = procedural_glyph('─', 1, &m).unwrap();
        // A horizontal line touches both side edges at the same height.
        let row = (0..h.height as usize)
            .find(|&y| h.coverage[y * h.width as usize] != 0)
            .unwrap();
        assert_eq!(
            h.coverage[row * h.width as usize + h.width as usize - 1],
            255
        );
        let v = procedural_glyph('│', 1, &m).unwrap();
        let col = (0..v.width as usize).find(|&x| v.coverage[x] != 0).unwrap();
        assert_ne!(
            v.coverage[(v.height as usize - 1) * v.width as usize + col],
            0
        );
        // Corners, crosses, double lines and blocks all draw something; dashes go to the font.
        for c in ['┌', '┼', '╋', '═', '╬', '╭', '█', '▀', '▄', '░', '▐', '▙']
        {
            assert!(procedural_glyph(c, 1, &m).unwrap().ink() > 0, "{c}");
        }
        assert!(procedural_glyph('┄', 1, &m).is_none());
        assert!(procedural_glyph('A', 1, &m).is_none());
        let full = procedural_glyph('█', 1, &m).unwrap();
        assert_eq!(full.ink() as u32, m.width * m.height);
        let half = procedural_glyph('▀', 1, &m).unwrap();
        assert_eq!(half.ink() as u32, m.width * (m.height / 2));
    }

    #[test]
    fn cmap_formats_4_and_12() {
        // A format 4 table mapping 'A'..='C' to 10..12 and U+AC00 through a glyph array.
        let mut f4 = Vec::new();
        let segs: [(u16, u16, i16, u16); 3] = [
            (0x43, 0x41, 10 - 0x41, 0),
            (0xAC00, 0xAC00, 0, 1),
            (0xFFFF, 0xFFFF, 1, 0),
        ];
        let n = segs.len() as u16;
        for v in [4u16, 0, 0, n * 2, 0, 0, 0] {
            f4.extend_from_slice(&v.to_be_bytes());
        }
        for s in &segs {
            f4.extend_from_slice(&s.0.to_be_bytes());
        }
        f4.extend_from_slice(&0u16.to_be_bytes());
        for s in &segs {
            f4.extend_from_slice(&s.1.to_be_bytes());
        }
        for s in &segs {
            f4.extend_from_slice(&s.2.to_be_bytes());
        }
        // idRangeOffset for segment 1 points at the glyph array right after the offsets.
        for (i, s) in segs.iter().enumerate() {
            let ro = if s.3 == 1 {
                ((segs.len() - i) * 2) as u16
            } else {
                0
            };
            f4.extend_from_slice(&ro.to_be_bytes());
        }
        f4.extend_from_slice(&77u16.to_be_bytes());
        let mut table = vec![0, 0, 0, 1, 0, 3, 0, 1, 0, 0, 0, 12];
        table.extend_from_slice(&f4);
        let cmap = Cmap::parse(table);
        assert_eq!(cmap.glyph('A'), 10);
        assert_eq!(cmap.glyph('C'), 12);
        assert_eq!(cmap.glyph('D'), 0);
        assert_eq!(cmap.glyph('가'), 77);
        assert_eq!(cmap.glyph('😀'), 0, "format 4 has no supplementary planes");
        // Format 12 with one group for the emoji block.
        let mut f12 = Vec::new();
        f12.extend_from_slice(&12u16.to_be_bytes());
        f12.extend_from_slice(&0u16.to_be_bytes());
        f12.extend_from_slice(&28u32.to_be_bytes());
        f12.extend_from_slice(&0u32.to_be_bytes());
        f12.extend_from_slice(&1u32.to_be_bytes());
        for v in [0x1F600u32, 0x1F64F, 500] {
            f12.extend_from_slice(&v.to_be_bytes());
        }
        let mut table = vec![0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12];
        table.extend_from_slice(&f12);
        let cmap = Cmap::parse(table);
        assert_eq!(cmap.glyph('😀'), 500);
        assert_eq!(cmap.glyph('😁'), 501);
        assert_eq!(cmap.glyph('A'), 0);
        assert_eq!(
            Cmap::parse(vec![1, 2]).glyph('A'),
            0,
            "garbage maps nothing"
        );
    }

    #[test]
    fn name_table_family_names() {
        // A Windows UTF-16BE family name, a subfamily (skipped) and a Mac Roman family name.
        let names = [
            ("Malgun Gothic", 3u16, 1u16),
            ("Regular", 3, 2),
            ("Mac Name", 1, 1),
        ];
        let mut storage = Vec::new();
        let mut records = Vec::new();
        for (text, platform, id) in names {
            let bytes: Vec<u8> = if platform == 3 {
                text.encode_utf16().flat_map(u16::to_be_bytes).collect()
            } else {
                text.bytes().collect()
            };
            for v in [platform, 0, 0, id, bytes.len() as u16, storage.len() as u16] {
                records.extend_from_slice(&v.to_be_bytes());
            }
            storage.extend(bytes);
        }
        let mut table = Vec::new();
        for v in [0u16, names.len() as u16, (6 + records.len()) as u16] {
            table.extend_from_slice(&v.to_be_bytes());
        }
        table.extend(records);
        table.extend(storage);
        assert_eq!(font_names(&table), ["Malgun Gothic", "Mac Name"]);
        assert!(font_names(&[0, 0]).is_empty());
    }

    #[test]
    fn the_cache_draws_once() {
        let m = metrics();
        let mut r = BoxRasterizer::new();
        r.configure(13, 16);
        let mut c = GlyphCache::default();
        let a = c.get(&mut r, &m, 'x', "", Style::default(), 1).clone();
        let b = c.get(&mut r, &m, 'x', "", Style::default(), 1).clone();
        assert_eq!(a, b);
        assert_eq!((c.hits, c.misses), (1, 1));
        c.get(&mut r, &m, 'e', "\u{301}", Style::default(), 1);
        assert_eq!(c.len(), 2);
        assert!(c.bytes() > 0);
    }
}
