//! The CPU cell-grid renderer (ADR 0036, docs/TERMINAL_EMULATOR.md §5).
//!
//! Draws a [`TermView`] into a [`Frame`] of `0x00RRGGBB` pixels: a fixed cell grid (East Asian
//! wide characters span two cells), colours from a [`Theme`], glyphs from a [`Rasterizer`] kept
//! in a [`GlyphCache`], box drawing and blocks drawn procedurally, decorations, hyperlinks, the
//! selection and the cursor. Only rows whose content changed since the last frame are drawn again
//! (each row is hashed), and [`Renderer::render`] returns those rows as [`Damage`] so the window
//! presents only them. Nothing runs between frames: no timer, no thread, no GPU context.
//!
//! The frame is presentation-agnostic: the Subak GUI blits it with GDI (`SetDIBitsToDevice`) on
//! Windows, tests compare it with goldens, and another toolkit could upload it as a texture.

pub mod glyph;
pub mod theme;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod gdi;

use crate::screen::{Attrs, CellWidth, Color, CursorShape, Row};
use crate::view::TermView;
pub use glyph::{
    BoxRasterizer, CellMetrics, FaceInfo, GlyphBitmap, GlyphCache, Rasterizer, Style,
    procedural_glyph,
};
use std::fmt;
use std::hash::{Hash, Hasher};
pub use theme::{Rgb, Theme};

/// The terminal font chain of docs/DESIGN_SYSTEM.md §2.3 (`mono.family` in design/tokens.json,
/// checked by a test). Families that are not installed are skipped.
pub const MONO_FAMILY: [&str; 6] = [
    "Cascadia Mono",
    "Consolas",
    "D2Coding",
    "Malgun Gothic",
    "Segoe UI Emoji",
    "Segoe UI Symbol",
];

/// Sizes and choices the renderer draws with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderOptions {
    /// Device pixels per logical pixel: 1.0 at 96 DPI, 1.25, 1.5, 1.75, 2.0 …
    pub scale: f32,
    /// The terminal font size in logical pixels (`size.terminal`).
    pub font_size: f32,
    /// The cell height as a multiple of the font size (`line-height.terminal`).
    pub line_height: f32,
    /// Inner padding of the terminal surface in logical pixels.
    pub padding: f32,
    /// Bold text in ANSI colours 0–7 uses their bright variants (as Windows Terminal does).
    pub bold_is_bright: bool,
}

impl Default for RenderOptions {
    /// The design tokens: 13 px, line height 1.2, 8 px padding, at 100 %.
    fn default() -> Self {
        Self {
            scale: 1.0,
            font_size: 13.0,
            line_height: 1.2,
            padding: 8.0,
            bold_is_bright: true,
        }
    }
}

/// A rendered picture: `width × height` pixels, top-down, each `0x00RRGGBB`.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Frame({}x{}, hash {:016x})",
            self.width,
            self.height,
            self.hash()
        )
    }
}

impl Frame {
    pub fn new(width: u32, height: u32, fill: Rgb) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill.to_u32(); (width as usize) * (height as usize)],
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> Rgb {
        Rgb::from_u32(self.pixels[(y * self.width + x) as usize])
    }

    /// Fills a rectangle, clipped to the frame.
    pub fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: Rgb) {
        let (x1, y1) = ((x + w).min(self.width), (y + h).min(self.height));
        let v = c.to_u32();
        for yy in y.min(y1)..y1 {
            let row = (yy * self.width) as usize;
            self.pixels[row + x.min(x1) as usize..row + x1 as usize].fill(v);
        }
    }

    /// Draws `g` at (`ox`, `oy`) in `fg` over what is there, inside `clip` (x0, y0, x1, y1).
    fn blend(&mut self, ox: i64, oy: i64, g: &GlyphBitmap, fg: Rgb, clip: [i64; 4]) {
        let (w, h) = (i64::from(self.width), i64::from(self.height));
        let (x0, y0) = (clip[0].max(0), clip[1].max(0));
        let (x1, y1) = (clip[2].min(w), clip[3].min(h));
        let gw = g.width as usize;
        for gy in 0..g.height as usize {
            let py = oy + gy as i64;
            if py < y0 || py >= y1 {
                continue;
            }
            for (gx, &a) in g.coverage[gy * gw..(gy + 1) * gw].iter().enumerate() {
                let px = ox + gx as i64;
                if a == 0 || px < x0 || px >= x1 {
                    continue;
                }
                let i = (py * w + px) as usize;
                self.pixels[i] = if a == 255 {
                    fg.to_u32()
                } else {
                    Rgb::from_u32(self.pixels[i]).mix(fg, a).to_u32()
                };
            }
        }
    }

    /// FNV-1a of the pixels (golden tests).
    pub fn hash(&self) -> u64 {
        let mut h = Fnv::default();
        h.write_u32(self.width);
        h.write_u32(self.height);
        for p in &self.pixels {
            h.write_u32(*p);
        }
        h.finish()
    }

    /// The frame as a binary PPM (P6) image, for looking at test failures and screenshots.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for p in &self.pixels {
            let c = Rgb::from_u32(*p);
            out.extend_from_slice(&[c.0, c.1, c.2]);
        }
        out
    }

    /// The frame as a 24-bit BMP image (opens in any Windows image viewer).
    pub fn to_bmp(&self) -> Vec<u8> {
        let row = (self.width as usize * 3).div_ceil(4) * 4;
        let size = 54 + row * self.height as usize;
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&u32::try_from(size).unwrap_or(0).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&self.width.to_le_bytes());
        b.extend_from_slice(&self.height.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        for y in (0..self.height).rev() {
            let start = b.len();
            for x in 0..self.width {
                let c = self.pixel(x, y);
                b.extend_from_slice(&[c.2, c.1, c.0]);
            }
            b.resize(start + row, 0);
        }
        b
    }

    /// Bytes of pixel memory.
    pub fn bytes(&self) -> usize {
        self.pixels.capacity() * 4
    }
}

/// FNV-1a, 64 bit: cheap and stable for row and frame hashes.
#[derive(Debug, Clone, Copy)]
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// The rows one [`Renderer::render`] drew, as half-open runs of row numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Damage {
    /// The whole frame was drawn (size, theme, scale or focus changed, or the first frame).
    pub full: bool,
    pub rows: Vec<(u16, u16)>,
}

impl Damage {
    pub fn is_empty(&self) -> bool {
        !self.full && self.rows.is_empty()
    }

    pub fn row_count(&self) -> usize {
        self.rows.iter().map(|(a, b)| usize::from(b - a)).sum()
    }
}

/// Counters for the benchmark harness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub frames: u64,
    pub rows_drawn: u64,
}

/// Draws one view into a frame; one per view (it caches the last frame and its rows' hashes).
#[derive(Debug)]
pub struct Renderer {
    raster: Box<dyn Rasterizer>,
    theme: Theme,
    opts: RenderOptions,
    metrics: CellMetrics,
    pad: u32,
    frame: Frame,
    size: (u16, u16),
    hashes: Vec<u64>,
    cache: GlyphCache,
    focused: bool,
    reverse: bool,
    full: bool,
    stats: RenderStats,
    /// Per-cell foreground colours of the row being drawn (kept to avoid an allocation per row).
    fgs: Vec<Rgb>,
}

impl Renderer {
    pub fn new(raster: Box<dyn Rasterizer>, theme: Theme, opts: RenderOptions) -> Self {
        let mut r = Self {
            raster,
            theme,
            opts,
            metrics: CellMetrics {
                width: 1,
                height: 1,
                baseline: 1,
                underline: 0,
                strike: 0,
                stroke: 1,
            },
            pad: 0,
            frame: Frame::new(0, 0, Rgb(0, 0, 0)),
            size: (0, 0),
            hashes: Vec::new(),
            cache: GlyphCache::default(),
            focused: true,
            reverse: false,
            full: true,
            stats: RenderStats::default(),
            fgs: Vec::new(),
        };
        r.configure();
        r
    }

    /// A renderer drawing with the deterministic built-in rasterizer (tests, other platforms).
    pub fn builtin(theme: Theme, opts: RenderOptions) -> Self {
        Self::new(Box::new(BoxRasterizer::new()), theme, opts)
    }

    /// A renderer drawing with the system's fonts: the [`MONO_FAMILY`] chain through GDI.
    #[cfg(windows)]
    pub fn with_system_fonts(theme: Theme, opts: RenderOptions) -> Result<Self, String> {
        let raster = gdi::GdiRasterizer::new(&MONO_FAMILY)?;
        Ok(Self::new(Box::new(raster), theme, opts))
    }

    /// System fonts are rasterized through GDI on Windows only; other platforms are not
    /// supported yet (`KIM-NFR-011`).
    #[cfg(not(windows))]
    pub fn with_system_fonts(_theme: Theme, _opts: RenderOptions) -> Result<Self, String> {
        Err("unsupported_platform: the system-font rasterizer exists for Windows only".into())
    }

    fn configure(&mut self) {
        let o = self.opts;
        let em = (o.font_size * o.scale).round().max(4.0) as u32;
        let cell_h = (o.font_size * o.line_height * o.scale).ceil().max(4.0) as u32;
        self.metrics = self.raster.configure(em, cell_h);
        self.pad = (o.padding * o.scale).round().max(0.0) as u32;
        self.cache.clear();
        self.size = (0, 0);
        self.full = true;
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn options(&self) -> RenderOptions {
        self.opts
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    pub fn rasterizer_name(&self) -> &str {
        self.raster.name()
    }

    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    pub fn glyph_cache(&self) -> &GlyphCache {
        &self.cache
    }

    /// Inner padding in device pixels.
    pub fn padding(&self) -> u32 {
        self.pad
    }

    /// Memory the renderer holds: the frame, the glyph cache and the row hashes.
    pub fn memory_bytes(&self) -> usize {
        self.frame.bytes() + self.cache.bytes() + self.hashes.capacity() * 8
    }

    /// The frame size for a grid of `cols × rows`.
    pub fn frame_size_for(&self, cols: u16, rows: u16) -> (u32, u32) {
        (
            u32::from(cols) * self.metrics.width + 2 * self.pad,
            u32::from(rows) * self.metrics.height + 2 * self.pad,
        )
    }

    /// The grid a surface of `width × height` device pixels holds (the size to ask the Host for).
    pub fn grid_size_for(&self, width: u32, height: u32) -> (u16, u16) {
        let fit = |px: u32, cell: u32| {
            u16::try_from((px.saturating_sub(2 * self.pad) / cell).max(1)).unwrap_or(u16::MAX)
        };
        (
            fit(width, self.metrics.width),
            fit(height, self.metrics.height),
        )
    }

    /// The cell under a pixel of the frame, if it is on the grid.
    pub fn cell_at(&self, x: u32, y: u32) -> Option<(u16, u16)> {
        let (gx, gy) = (x.checked_sub(self.pad)?, y.checked_sub(self.pad)?);
        let (c, r) = (gx / self.metrics.width, gy / self.metrics.height);
        (c < u32::from(self.size.0) && r < u32::from(self.size.1)).then_some((c as u16, r as u16))
    }

    /// Switches the theme; the next frame is drawn in full.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.full = true;
    }

    /// Changes the scale (a DPI change): glyphs are rasterized again at the new size, never
    /// scaled as bitmaps.
    pub fn set_scale(&mut self, scale: f32) {
        self.opts.scale = scale;
        self.configure();
    }

    /// Changes the terminal text size in logical pixels (the user's zoom, Step 16): glyphs are
    /// rasterized again at the new size and the next frame is drawn in full.
    pub fn set_font_size(&mut self, size: f32) {
        self.opts.font_size = size;
        self.configure();
    }

    /// The window gained or lost focus (the cursor becomes hollow when it has none).
    pub fn set_focused(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.full = true;
        }
    }

    /// Which face draws `ch` (procedural for box drawing and blocks).
    pub fn face_for(&mut self, ch: char) -> FaceInfo {
        if procedural_glyph(ch, 1, &self.metrics).is_some() {
            return FaceInfo {
                family: "procedural".into(),
                fallback: false,
                found: true,
            };
        }
        self.raster.face_for(ch)
    }

    /// Draws what changed in `view` since the last frame.
    pub fn render(&mut self, view: &TermView) -> Damage {
        let (cols, rows) = view.size();
        let screen = view.screen();
        let modes = screen.modes();
        if (cols, rows) != self.size {
            self.size = (cols, rows);
            let (w, h) = self.frame_size_for(cols, rows);
            self.frame = Frame::new(w, h, self.theme.background);
            self.hashes = vec![0; usize::from(rows)];
            self.full = true;
        }
        if modes.reverse_video != self.reverse {
            self.reverse = modes.reverse_video;
            self.full = true;
        }
        let full = std::mem::take(&mut self.full);
        if full {
            let bg = if self.reverse {
                self.theme.foreground
            } else {
                self.theme.background
            };
            self.frame.pixels.fill(bg.to_u32());
        }
        let (shape, _) = screen.cursor_shape();
        let cursor = if modes.cursor_visible {
            view.cursor_in_view()
        } else {
            None
        };
        let mut damage = Damage {
            full,
            rows: Vec::new(),
        };
        for r in 0..rows {
            let row = view.row_at(r);
            let sel = view.selection_cols(r);
            let cur = cursor.filter(|c| c.1 == r).map(|c| c.0);
            let mut h = Fnv::default();
            hash_row(&row, &mut h);
            (sel, cur, shape as u8, self.focused).hash(&mut h);
            let h = h.finish();
            let i = usize::from(r);
            if !full && self.hashes[i] == h {
                continue;
            }
            self.hashes[i] = h;
            self.draw_row(r, &row, sel, cur, shape);
            match damage.rows.last_mut() {
                Some((_, end)) if *end == r => *end = r + 1,
                _ => damage.rows.push((r, r + 1)),
            }
        }
        self.stats.frames += 1;
        self.stats.rows_drawn += damage.row_count() as u64;
        damage
    }

    /// Foreground and background of a cell.
    fn colors(&self, a: Attrs, selected: bool) -> (Rgb, Rgb) {
        let t = &self.theme;
        let (dfg, dbg) = if self.reverse {
            (t.background, t.foreground)
        } else {
            (t.foreground, t.background)
        };
        let mut fg = match a.fg {
            Color::Default => dfg,
            Color::Indexed(n) => {
                let n = if self.opts.bold_is_bright && a.bold() && n < 8 {
                    n + 8
                } else {
                    n
                };
                t.indexed(n)
            }
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        };
        let mut bg = match a.bg {
            Color::Default => dbg,
            Color::Indexed(n) => t.indexed(n),
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        };
        if a.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if a.dim() {
            fg = fg.mix(bg, 102);
        }
        if a.hidden() {
            fg = bg;
        }
        if selected {
            bg = t.selection;
        }
        (fg, bg)
    }

    fn draw_row(
        &mut self,
        r: u16,
        row: &Row,
        sel: Option<(u16, u16)>,
        cursor: Option<u16>,
        shape: CursorShape,
    ) {
        let m = self.metrics;
        let (cw, ch) = (m.width, m.height);
        let cols = usize::from(self.size.0);
        let x0 = self.pad;
        let y0 = self.pad + u32::from(r) * ch;
        // The cells the cursor covers: a wide character's two.
        let cursor_span = cursor.map(|cx| {
            let cx = usize::from(cx);
            match row.cell(cx).width {
                CellWidth::Wide => (cx, 2),
                CellWidth::Tail if cx > 0 => (cx - 1, 2),
                _ => (cx, 1),
            }
        });
        let block = cursor_span.filter(|_| shape == CursorShape::Block && self.focused);
        // 1. Backgrounds (a focused block cursor inverts its cells).
        self.fgs.clear();
        for x in 0..cols {
            let c = row.cell(x);
            let selected = sel.is_some_and(|(a, b)| (usize::from(a)..=usize::from(b)).contains(&x));
            let (mut fg, mut bg) = self.colors(c.attrs, selected);
            if let Some((start, n)) = block
                && (start..start + n).contains(&x)
            {
                bg = self.theme.cursor;
                fg = self.theme.background;
            }
            self.frame.fill(x0 + x as u32 * cw, y0, cw, ch, bg);
            self.fgs.push(fg);
        }
        // 2. Glyphs, clipped to the row (italic and fallback glyphs may overhang a neighbour).
        let clip = [
            i64::from(x0),
            i64::from(y0),
            i64::from(x0) + (cols as i64) * i64::from(cw),
            i64::from(y0 + ch),
        ];
        for x in 0..cols {
            let c = row.cell(x);
            if c.width == CellWidth::Tail || c.attrs.hidden() {
                continue;
            }
            if c.ch == ' ' && c.combining.is_empty() {
                continue;
            }
            let cells = if c.width == CellWidth::Wide { 2 } else { 1 };
            let style = Style {
                bold: c.attrs.bold(),
                italic: c.attrs.italic(),
            };
            let g = self
                .cache
                .get(&mut *self.raster, &m, c.ch, c.combining, style, cells);
            let ox = i64::from(x0) + x as i64 * i64::from(cw) + i64::from(g.left);
            self.frame
                .blend(ox, i64::from(y0) + i64::from(g.top), g, self.fgs[x], clip);
        }
        // 3. Decorations.
        let s = m.stroke;
        for x in 0..cols {
            let c = row.cell(x);
            let a = c.attrs;
            if a.hidden() {
                continue;
            }
            let (fg, xx) = (self.fgs[x], x0 + x as u32 * cw);
            let under = a.underline();
            if under >= 1 {
                self.frame.fill(xx, y0 + m.underline, cw, s, fg);
            }
            if under == 2 {
                let y2 = (m.underline + 2 * s).min(ch - s);
                self.frame.fill(xx, y0 + y2, cw, s, fg);
            }
            if a.strike() {
                self.frame.fill(xx, y0 + m.strike, cw, s, fg);
            }
            if a.overline() {
                self.frame.fill(xx, y0, cw, s, fg);
            }
            if c.link.is_some() && under == 0 {
                // A dotted underline marks a hyperlink.
                let mut px = 0;
                while px < cw {
                    self.frame.fill(xx + px, y0 + m.underline, s, s, fg);
                    px += 2 * s;
                }
            }
        }
        // 4. The other cursor shapes, and the hollow box of an unfocused window.
        if let Some((start, n)) = cursor_span {
            let c = self.theme.cursor;
            let (xx, w) = (x0 + start as u32 * cw, n as u32 * cw);
            let t = (2 * s).max(2);
            if !self.focused {
                self.frame.fill(xx, y0, w, s, c);
                self.frame.fill(xx, y0 + ch - s, w, s, c);
                self.frame.fill(xx, y0, s, ch, c);
                self.frame.fill(xx + w - s, y0, s, ch, c);
            } else if shape == CursorShape::Underline {
                self.frame.fill(xx, y0 + ch - t, w, t, c);
            } else if shape == CursorShape::Bar {
                self.frame.fill(xx, y0, t, ch, c);
            }
        }
    }
}

/// Everything about a row that changes its pixels.
fn hash_row(row: &Row, h: &mut Fnv) {
    for x in 0..row.cols() {
        let c = row.cell(x);
        h.write_u32(u32::from(c.ch));
        h.write_u8(c.width as u8);
        c.attrs.hash(h);
        if !c.combining.is_empty() {
            h.write(c.combining.as_bytes());
        }
        h.write_u8(u8::from(c.link.is_some()));
    }
}
