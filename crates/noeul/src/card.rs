//! Cards: text, HTML, colour values and file names rendered into a picture that pins, transforms,
//! copies, saves and closes like any image pin (Step 55; NOEUL-FR-071, NOEUL-SEC-009; ADR 0047).
//!
//! A card is built in two steps, both platform-neutral:
//!
//! 1. **A document** ([`Doc`]): blocks (paragraphs, headings, list items, preformatted text,
//!    rules) of styled runs. Plain text becomes one per line ([`Doc::from_text`]); clipboard HTML
//!    goes through Noeul's own sanitizer (`crate::html`), which keeps text and basic formatting
//!    only; file names become a list ([`Doc::files`]).
//! 2. **Layout and pixels** ([`render`]): Noeul's own line breaking at the monitor's scale (a
//!    content width of at most [`MAX_WIDTH`] logical pixels, words wrapped, overlong words broken),
//!    then every run's glyph coverage from a [`TextEngine`] (GDI with per-character font fallback
//!    on Windows, `win::card`; block glyphs in tests) blended in its colour onto the card. No
//!    browser engine, no script, no style sheet, no network: an image the HTML names is a
//!    placeholder, never fetched.
//!
//! **Colours are checked** ([`legible`]): a run whose colour has less than 3:1 contrast with what
//! is behind it (white on white, `color: transparent`, a colour equal to the background) is drawn
//! in the card's text colour instead, so no text can be hidden on a card. The card is on the Kim
//! dark surface (`bg.overlay`, `text.primary`, links in `accent`) unless the HTML gives its own
//! background (an editor's dark or light theme), whose text then defaults to black or white.
//!
//! **Bounds.** At most [`MAX_CHARS`] characters and [`MAX_LINES`] lines are drawn; a longer text
//! ends with a note saying how much was left out. A colour card ([`swatch`]) is a square of the
//! colour (on a checkerboard when it is translucent) with its HEX and RGB values.

use crate::frame::{Frame, FrameError, Ledger, Rect};
use crate::paste::Rgba;
use std::sync::Arc;

/// An opaque colour, `[r, g, b]`.
pub type Rgb = [u8; 3];

/// `bg.overlay` (DESIGN_SYSTEM §2): the card's surface.
pub const SURFACE: Rgb = [0x1C, 0x1B, 0x24];
/// `text.primary`.
pub const INK: Rgb = [0xE8, 0xE6, 0xF0];
/// `text.secondary`: placeholders and notes.
pub const INK_SOFT: Rgb = [0xA9, 0xA6, 0xB8];
/// `accent`: links.
pub const ACCENT: Rgb = [0xB7, 0xA6, 0xF5];
/// `border.strong`: rules and the quote bar.
pub const LINE: Rgb = [0x6A, 0x66, 0x80];

/// The most content width, in logical pixels (longer lines wrap; PASTE-11).
pub const MAX_WIDTH: u32 = 560;
/// The least content width, in logical pixels.
pub const MIN_WIDTH: u32 = 24;
/// The padding around the content, in logical pixels.
pub const PADDING: u32 = 12;
/// The most characters a card draws.
pub const MAX_CHARS: usize = 20_000;
/// The most lines a card draws.
pub const MAX_LINES: usize = 240;

/// One font of a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Font {
    /// The em height in device pixels.
    pub px: u32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

/// Glyph coverage: one byte per pixel, 0 (none) to 255 (full), `width × height`, the baseline at
/// the font's ascent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// What a card needs from the platform's text renderer.
pub trait TextEngine {
    /// `(ascent, descent)` of the font in device pixels.
    fn metrics(&mut self, font: Font) -> (u32, u32);
    /// The advance width of `text` (no line breaks) in device pixels.
    fn width(&mut self, text: &str, font: Font) -> u32;
    /// The coverage of `text` with its baseline at the font's ascent; at least as wide as its
    /// advance and as high as ascent + descent.
    fn coverage(&mut self, text: &str, font: Font) -> Mask;
}

/// Block glyphs: every character is a box `0.6 em` wide (tests and the fallback when GDI cannot
/// create a font).
#[derive(Debug, Default)]
pub struct BlockText;

impl TextEngine for BlockText {
    fn metrics(&mut self, font: Font) -> (u32, u32) {
        let a = (font.px * 8 / 10).max(1);
        (a, (font.px / 5).max(1))
    }

    fn width(&mut self, text: &str, font: Font) -> u32 {
        let per = (font.px * 6 / 10).max(1);
        u32::try_from(text.chars().count())
            .unwrap_or(u32::MAX)
            .saturating_mul(per)
    }

    fn coverage(&mut self, text: &str, font: Font) -> Mask {
        let (a, d) = self.metrics(font);
        let per = (font.px * 6 / 10).max(1);
        let width = self.width(text, font).max(1);
        let height = a + d;
        let mut data = vec![0u8; (width * height) as usize];
        for (i, c) in text.chars().enumerate() {
            if c.is_whitespace() {
                continue;
            }
            let x0 = i as u32 * per;
            for y in a / 4..a {
                for x in x0..(x0 + per.saturating_sub(1)).min(width) {
                    data[(y * width + x) as usize] = 255;
                }
            }
        }
        Mask {
            width,
            height,
            data,
        }
    }
}

/// Size steps of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Size {
    Small,
    #[default]
    Normal,
    /// Heading levels 1–6 (1 is the largest).
    Heading(u8),
}

/// How a run looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub link: bool,
    /// Stands for something not shown (an image the HTML names): drawn as a soft note.
    pub placeholder: bool,
    pub color: Option<Rgb>,
    /// A highlight behind the run (`<mark>`, an inline `background-color`).
    pub highlight: Option<Rgb>,
    pub size: Size,
}

/// One run of styled text. `"\n"` is a line break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

/// What a block is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// Level 1–6.
    Heading(u8),
    /// Whitespace kept (lines still wrap at the card's width).
    Preformatted,
    /// A list item with its marker ("•", "3.").
    Item(String),
    /// A horizontal rule.
    Rule,
}

/// One block of a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    /// Nesting (lists, quotes), in steps of 20 logical pixels.
    pub indent: u8,
    /// Inside a quotation: a bar on the left.
    pub quote: bool,
    pub runs: Vec<Run>,
}

impl Block {
    pub fn new(kind: BlockKind) -> Block {
        Block {
            kind,
            indent: 0,
            quote: false,
            runs: Vec::new(),
        }
    }

    pub fn chars(&self) -> usize {
        self.runs.iter().map(|r| r.text.chars().count()).sum()
    }
}

/// A card's content.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// The card's own background (from the HTML); `None`: the Kim surface.
    pub background: Option<Rgb>,
    /// Content was left out (a limit was reached); the card ends with a note.
    pub truncated: bool,
    /// Images the HTML named that are shown as placeholders.
    pub placeholders: usize,
}

/// Removes characters a card must not draw: C0 controls (but tabs and line breaks) and the
/// bidirectional embeddings, overrides and isolates that can reorder what is shown
/// (U+202A–U+202E, U+2066–U+2069).
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| {
            let u = *c as u32;
            !((u < 0x20 && *c != '\t' && *c != '\n')
                || u == 0x7F
                || (0x202A..=0x202E).contains(&u)
                || (0x2066..=0x2069).contains(&u))
        })
        .collect()
}

impl Doc {
    /// Plain text: one block per line, whitespace kept, tabs as four spaces. Monospace when the
    /// text looks like code or a table (tabs, or a quarter of its lines indented).
    pub fn from_text(text: &str) -> Doc {
        let text = clean(&text.replace("\r\n", "\n").replace('\r', "\n"));
        let lines: Vec<&str> = text.split('\n').collect();
        let content: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
        let indented = content
            .iter()
            .filter(|l| l.starts_with([' ', '\t']))
            .count();
        let mono = text.contains('\t') || (indented > 0 && indented * 4 >= content.len());
        let mut doc = Doc::default();
        let mut chars = 0usize;
        // Trailing empty lines add nothing.
        let end = lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map_or(0, |i| i + 1);
        for line in &lines[..end] {
            let line = line.replace('\t', "    ");
            let n = line.chars().count();
            if chars + n > MAX_CHARS || doc.blocks.len() >= MAX_LINES {
                doc.truncated = true;
                break;
            }
            chars += n;
            let mut b = Block::new(BlockKind::Preformatted);
            b.runs.push(Run {
                text: line,
                style: Style {
                    mono,
                    ..Style::default()
                },
            });
            doc.blocks.push(b);
        }
        doc
    }

    /// File names (never their folders): a heading and one item per name.
    pub fn files(names: &[String], more: usize) -> Doc {
        let mut doc = Doc::default();
        let total = names.len() + more;
        let mut h = Block::new(BlockKind::Paragraph);
        h.runs.push(Run {
            text: if total == 1 {
                "1 copied file".into()
            } else {
                format!("{total} copied files")
            },
            style: Style {
                bold: true,
                ..Style::default()
            },
        });
        doc.blocks.push(h);
        for n in names {
            let mut b = Block::new(BlockKind::Item("\u{2022}".into()));
            b.runs.push(Run {
                text: clean(n),
                style: Style::default(),
            });
            doc.blocks.push(b);
        }
        if more > 0 {
            doc.truncated = true;
        }
        doc
    }

    /// Every character of the document's text (`pin.copy-text`): blocks on their own lines.
    pub fn plain_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for b in self.blocks.iter().filter(|b| b.kind != BlockKind::Rule) {
            let mut line = String::new();
            if let BlockKind::Item(m) = &b.kind {
                line.push_str(m);
                line.push(' ');
            }
            for r in b.runs.iter().filter(|r| !r.style.placeholder) {
                line.push_str(&r.text);
            }
            // A block ending in a line break (`<div><br></div>`) is one line.
            if line.ends_with('\n') {
                line.pop();
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    pub fn chars(&self) -> usize {
        self.blocks.iter().map(Block::chars).sum()
    }
}

/// WCAG relative luminance of an sRGB colour.
pub fn luminance(c: Rgb) -> f64 {
    let ch = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.040_45 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(c[0]) + 0.7152 * ch(c[1]) + 0.0722 * ch(c[2])
}

/// WCAG contrast ratio of two colours (1 to 21).
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// The least contrast a run keeps with what is behind it.
pub const MIN_CONTRAST: f64 = 3.0;

/// The default text colour on `bg`.
pub fn ink_on(bg: Rgb) -> Rgb {
    if bg == SURFACE {
        INK
    } else if luminance(bg) > 0.18 {
        [0x14, 0x13, 0x1A]
    } else {
        [0xF4, 0xF3, 0xF8]
    }
}

/// `want` when it is legible on `bg`, else the default text colour there.
pub fn legible(want: Rgb, bg: Rgb) -> Rgb {
    if contrast(want, bg) >= MIN_CONTRAST {
        want
    } else {
        ink_on(bg)
    }
}

/// Logical pixels at `dpi`.
fn px(dpi: u32, logical: u32) -> u32 {
    (logical * dpi.max(48) + 48) / 96
}

fn font_of(style: &Style, kind: &BlockKind, dpi: u32) -> Font {
    let (size, bold) = match (kind.clone(), style.size) {
        (BlockKind::Heading(l), _) | (_, Size::Heading(l)) => match l {
            1 => (24, true),
            2 => (20, true),
            3 => (17, true),
            _ => (15, true),
        },
        (_, Size::Small) => (12, false),
        _ => (14, false),
    };
    Font {
        px: px(dpi, size),
        bold: bold || style.bold,
        italic: style.italic || style.placeholder,
        mono: style.mono,
    }
}

/// One piece of text placed on a line.
#[derive(Debug, Clone)]
struct Piece {
    x: u32,
    width: u32,
    text: String,
    font: Font,
    style: Style,
}

#[derive(Debug, Default)]
struct Line {
    pieces: Vec<Piece>,
    /// A rule line: no text, a horizontal line.
    rule: bool,
    quote_x: Option<u32>,
    ascent: u32,
    descent: u32,
}

/// The pieces of a run: words with their trailing spaces, or (whitespace kept) runs of spaces
/// and runs of other characters; `"\n"` stays alone.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev_space = false;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            if start < i {
                out.push(&text[start..i]);
            }
            out.push("\n");
            start = i + 1;
            prev_space = false;
            continue;
        }
        let space = c == ' ';
        if !space && prev_space && start < i {
            out.push(&text[start..i]);
            start = i;
        }
        prev_space = space;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

struct Layout<'a> {
    engine: &'a mut dyn TextEngine,
    dpi: u32,
    left: u32,
    right: u32,
    lines: Vec<Line>,
    line: Line,
    x: u32,
    widest: u32,
    full: bool,
    max_lines: usize,
}

impl Layout<'_> {
    fn flush(&mut self, force: bool) {
        if self.lines.len() >= self.max_lines {
            self.full = true;
            self.line = Line::default();
            return;
        }
        if force || !self.line.pieces.is_empty() || self.line.rule {
            let mut l = std::mem::take(&mut self.line);
            if l.ascent == 0 {
                let f = font_of(&Style::default(), &BlockKind::Paragraph, self.dpi);
                let (a, d) = self.engine.metrics(f);
                l.ascent = a;
                l.descent = d;
            }
            if let Some(p) = l.pieces.last() {
                self.widest = self.widest.max(p.x + p.width);
            }
            self.lines.push(l);
        }
        self.x = self.left;
    }

    fn push(&mut self, text: &str, font: Font, style: Style, keep_spaces: bool) {
        if self.full {
            return;
        }
        let w = self.engine.width(text, font);
        if self.x + w > self.right && self.x > self.left {
            if !keep_spaces && text.trim().is_empty() {
                return; // no space at the start of a wrapped line
            }
            self.flush(false);
        }
        let text = if !keep_spaces && self.x == self.left {
            text.trim_start()
        } else {
            text
        };
        if text.is_empty() {
            return;
        }
        let w = self.engine.width(text, font);
        if w > self.right - self.left {
            // A word wider than the card: break it between characters.
            let mut piece = String::new();
            for c in text.chars() {
                piece.push(c);
                if self.x + self.engine.width(&piece, font) > self.right
                    && piece.chars().count() > 1
                {
                    piece.pop();
                    let pw = self.engine.width(&piece, font);
                    self.place(std::mem::take(&mut piece), pw, font, style);
                    self.flush(false);
                    piece.push(c);
                }
            }
            if !piece.is_empty() {
                let pw = self.engine.width(&piece, font);
                self.place(piece, pw, font, style);
            }
            return;
        }
        self.place(text.to_string(), w, font, style);
    }

    fn place(&mut self, text: String, width: u32, font: Font, style: Style) {
        let (a, d) = self.engine.metrics(font);
        self.line.ascent = self.line.ascent.max(a);
        self.line.descent = self.line.descent.max(d);
        // Pieces of one style that touch are drawn together.
        if let Some(last) = self.line.pieces.last_mut()
            && last.font == font
            && last.style == style
            && last.x + last.width == self.x
        {
            last.text.push_str(&text);
            last.width += width;
        } else {
            self.line.pieces.push(Piece {
                x: self.x,
                width,
                text,
                font,
                style,
            });
        }
        self.x += width;
    }
}

/// The card's background: the document's own, or the Kim surface.
fn background(doc: &Doc) -> Rgb {
    doc.background.unwrap_or(SURFACE)
}

/// Renders `doc` as a card at `dpi`: an opaque picture at (0, 0).
pub fn render(
    ledger: &Arc<Ledger>,
    doc: &Doc,
    dpi: u32,
    engine: &mut dyn TextEngine,
) -> Result<Frame, FrameError> {
    let pad = px(dpi, PADDING);
    let content = px(dpi, MAX_WIDTH);
    let mut l = Layout {
        engine,
        dpi,
        left: pad,
        right: pad + content,
        lines: Vec::new(),
        line: Line::default(),
        x: pad,
        widest: pad + px(dpi, MIN_WIDTH),
        full: false,
        max_lines: MAX_LINES,
    };
    let gap = px(dpi, 6);
    let mut tops: Vec<(usize, u32)> = Vec::new(); // (first line of a block, gap before it)
    let mut drawn = 0usize;
    let mut cut = doc.truncated;
    let mut prev: Option<&BlockKind> = None;
    for b in &doc.blocks {
        if l.full {
            cut = true;
            break;
        }
        // Lines of preformatted text and items of a list stay together.
        let before = match (prev, &b.kind) {
            (None, _) => 0,
            (Some(BlockKind::Preformatted), BlockKind::Preformatted) => 0,
            (Some(BlockKind::Item(_)), BlockKind::Item(_)) => px(dpi, 2),
            _ => gap,
        };
        prev = Some(&b.kind);
        tops.push((l.lines.len(), before));
        let first = l.lines.len();
        let indent = px(dpi, 20) * u32::from(b.indent.min(8));
        let quote = b.quote.then(|| pad + indent.saturating_sub(px(dpi, 10)));
        l.left = (pad + indent).min(l.right.saturating_sub(px(dpi, MIN_WIDTH)));
        l.x = l.left;
        l.line.quote_x = quote;
        if b.kind == BlockKind::Rule {
            l.line.rule = true;
            l.flush(true);
            continue;
        }
        if let BlockKind::Item(marker) = &b.kind {
            let f = font_of(&Style::default(), &b.kind, dpi);
            let mw = l.engine.width(marker, f);
            let hang = px(dpi, 18).max(mw + px(dpi, 4));
            let mx = l.left;
            l.left = (l.left + hang).min(l.right.saturating_sub(px(dpi, MIN_WIDTH)));
            l.x = mx;
            l.place(marker.clone(), mw, f, Style::default());
            l.x = l.left;
        }
        let keep = b.kind == BlockKind::Preformatted;
        for r in &b.runs {
            let font = font_of(&r.style, &b.kind, dpi);
            for t in tokens(&r.text) {
                if drawn >= MAX_CHARS {
                    cut = true;
                    break;
                }
                if t == "\n" {
                    l.flush(true);
                    l.line.quote_x = quote;
                    continue;
                }
                drawn += t.chars().count();
                l.push(t, font, r.style, keep);
                l.line.quote_x = quote;
            }
        }
        // A block ending in a line break adds no extra empty line.
        if !l.line.pieces.is_empty() || l.lines.len() == first {
            l.flush(true);
        }
    }
    if cut {
        let mut note = Block::new(BlockKind::Paragraph);
        note.runs.push(Run {
            text: "\u{2026} more is not shown".into(),
            style: Style {
                placeholder: true,
                size: Size::Small,
                ..Style::default()
            },
        });
        l.full = false;
        l.max_lines = l.lines.len() + 1;
        l.left = pad;
        l.x = pad;
        tops.push((l.lines.len(), gap));
        let f = font_of(&note.runs[0].style, &note.kind, dpi);
        l.push(&note.runs[0].text, f, note.runs[0].style, false);
        l.flush(true);
    }
    let Layout {
        lines,
        widest,
        engine,
        ..
    } = l;
    // Vertical positions.
    let lead = px(dpi, 4);
    let mut ys = Vec::with_capacity(lines.len());
    let mut y = pad;
    let mut next_top = tops.iter().peekable();
    for (i, line) in lines.iter().enumerate() {
        while let Some((first, g)) = next_top.peek() {
            if *first == i {
                y += g;
                next_top.next();
            } else {
                break;
            }
        }
        ys.push(y);
        y += if line.rule {
            px(dpi, 9)
        } else {
            line.ascent + line.descent + lead
        };
    }
    let width = (widest + pad).max(2 * pad + px(dpi, MIN_WIDTH));
    let height = (y.saturating_sub(lead) + pad).max(2 * pad + 1);
    crate::frame::check_size(width, height)?;
    let bg = background(doc);
    let mut canvas = Canvas::new(width, height, bg);
    for (line, top) in lines.iter().zip(ys) {
        let tall = line.ascent + line.descent;
        if let Some(qx) = line.quote_x {
            canvas.fill(qx, top, px(dpi, 3).max(1), tall + lead, LINE);
        }
        if line.rule {
            canvas.fill(
                pad,
                top + px(dpi, 4),
                width.saturating_sub(2 * pad),
                px(dpi, 1).max(1),
                LINE,
            );
            continue;
        }
        let base = top + line.ascent;
        for p in &line.pieces {
            let behind = p.style.highlight.unwrap_or(bg);
            if let Some(h) = p.style.highlight {
                canvas.fill(p.x, top, p.width, tall, h);
            }
            let want = if p.style.placeholder {
                if behind == SURFACE {
                    INK_SOFT
                } else {
                    ink_on(behind)
                }
            } else if p.style.link && p.style.color.is_none() {
                if behind == SURFACE {
                    ACCENT
                } else {
                    ink_on(behind)
                }
            } else {
                p.style.color.unwrap_or_else(|| ink_on(behind))
            };
            let ink = legible(want, behind);
            let (a, _) = engine.metrics(p.font);
            let mask = engine.coverage(&p.text, p.font);
            canvas.blend(&mask, i64::from(p.x), i64::from(base) - i64::from(a), ink);
            let thick = (p.font.px / 14).max(1);
            if p.style.underline || p.style.link {
                canvas.fill(p.x, base + thick.max(p.font.px / 10), p.width, thick, ink);
            }
            if p.style.strike {
                canvas.fill(p.x, base.saturating_sub(a * 3 / 10), p.width, thick, ink);
            }
        }
    }
    Frame::new(ledger, Rect::new(0, 0, width, height), canvas.pixels)
}

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    /// An opaque canvas of one colour.
    fn new(width: u32, height: u32, bg: Rgb) -> Canvas {
        let n = u64::from(width) * u64::from(height);
        let mut pixels = Vec::with_capacity(usize::try_from(n * 4).unwrap_or(0));
        for _ in 0..n {
            pixels.extend_from_slice(&[bg[2], bg[1], bg[0], 255]);
        }
        Canvas {
            width,
            height,
            pixels,
        }
    }

    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: Rgb) {
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                let i = ((yy * self.width + xx) * 4) as usize;
                self.pixels[i..i + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
            }
        }
    }

    /// Blends `mask` in colour `c` with its top-left at (`x`, `y`).
    fn blend(&mut self, mask: &Mask, x: i64, y: i64, c: Rgb) {
        for my in 0..mask.height {
            let yy = y + i64::from(my);
            if yy < 0 || yy >= i64::from(self.height) {
                continue;
            }
            for mx in 0..mask.width {
                let xx = x + i64::from(mx);
                if xx < 0 || xx >= i64::from(self.width) {
                    continue;
                }
                let a = u32::from(mask.data[(my * mask.width + mx) as usize]);
                if a == 0 {
                    continue;
                }
                let i = ((yy as u32 * self.width + xx as u32) * 4) as usize;
                for (k, ch) in [c[2], c[1], c[0]].into_iter().enumerate() {
                    let old = u32::from(self.pixels[i + k]);
                    self.pixels[i + k] = ((old * (255 - a) + u32::from(ch) * a + 127) / 255) as u8;
                }
            }
        }
    }
}

/// A colour card (Step 55, PASTE-04): a square of the colour (on a checkerboard when it is
/// translucent) above its HEX and RGB values.
pub fn swatch(
    ledger: &Arc<Ledger>,
    color: Rgba,
    dpi: u32,
    engine: &mut dyn TextEngine,
) -> Result<Frame, FrameError> {
    let pad = px(dpi, PADDING);
    let side = px(dpi, 112);
    let font = Font {
        px: px(dpi, 14),
        bold: true,
        italic: false,
        mono: true,
    };
    let small = Font {
        px: px(dpi, 12),
        bold: false,
        ..font
    };
    let hex = color.hex();
    let css = color.css();
    let (ha, hd) = engine.metrics(font);
    let (sa, sd) = engine.metrics(small);
    let text_w = engine.width(&hex, font).max(engine.width(&css, small));
    let width = 2 * pad + side.max(text_w);
    let gap = px(dpi, 8);
    let height = pad + side + gap + ha + hd + px(dpi, 2) + sa + sd + pad;
    crate::frame::check_size(width, height)?;
    let mut canvas = Canvas::new(width, height, SURFACE);
    let cell = px(dpi, 8).max(1);
    let a = u32::from(color.a);
    for y in 0..side {
        for x in 0..side {
            let light = ((x / cell) + (y / cell)).is_multiple_of(2);
            let under: Rgb = if light { [0xFF; 3] } else { [0xC8; 3] };
            let mix =
                |c: u8, u: u8| ((u32::from(c) * a + u32::from(u) * (255 - a) + 127) / 255) as u8;
            let px_ = [
                mix(color.r, under[0]),
                mix(color.g, under[1]),
                mix(color.b, under[2]),
            ];
            canvas.fill(pad + x, pad + y, 1, 1, px_);
        }
    }
    let top = pad + side + gap;
    let m = engine.coverage(&hex, font);
    canvas.blend(&m, i64::from(pad), i64::from(top), INK);
    let m = engine.coverage(&css, small);
    canvas.blend(
        &m,
        i64::from(pad),
        i64::from(top + ha + hd + px(dpi, 2)),
        INK_SOFT,
    );
    Frame::new(ledger, Rect::new(0, 0, width, height), canvas.pixels)
}

#[cfg(test)]
mod tests;
