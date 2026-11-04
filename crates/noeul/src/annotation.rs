//! Annotation: shapes, freehand strokes and text labels as editable vector objects (Step 56;
//! NOEUL-FR-080, NOEUL-FR-081, NOEUL-FR-083, NOEUL-NFR-014; NOEUL_UX_SPEC §4.9; ADR 0038).
//!
//! Everything here is platform-independent and runs headless:
//!
//! - **The model.** An annotation is an [`Object`]: a [`Shape`] (rectangle, ellipse, line, arrow,
//!   pen stroke, marker stroke or text label), a colour from the [`PALETTE`], a width level
//!   ([`WIDTHS`]) and the DPI of the monitor it was drawn on, so its strokes are as thick in device
//!   pixels as they looked on that monitor. Coordinates are device pixels of the picture it
//!   annotates (the frozen picture in the overlay, the pin's image on a pin), never screen points
//!   of a window that may move. Objects stay vectors until an output needs pixels
//!   ([`flatten`]: copy, save, a new pin's window); nothing is painted into the picture before.
//! - **Undo.** A [`Doc`] keeps the objects in drawing order (the last is on top) and every edit
//!   as one undo step — a whole stroke, a whole move, a whole text label typed — with redo until
//!   the next edit (NOEUL-FR-081). "Clear all" is an ordinary step. Undo and redo restore the
//!   exact objects, so they are lossless by construction.
//! - **Editing.** [`Editor`] turns presses, drags, keys, characters and input-method events into
//!   edits: draw with the current [`Tool`], select an object by pressing on it (the pen and the
//!   marker always draw; the text tool edits the label it presses on), move it, resize it by its
//!   handles, reorder, recolour, re-width or delete it, type and compose text (Korean IME
//!   compositions are shown inline and become text only when committed), and draw with the
//!   keyboard (`selection.mark` sets the points). It records which pixels changed
//!   ([`Editor::take_dirty`]) so the overlay redraws only those.
//! - **Pixels.** [`draw`] rasterizes objects into a BGRA buffer: anti-aliased strokes (round
//!   caps and joins), crisp axis-aligned rectangles, arrow heads, a marker's single translucent
//!   coverage (no darker overlaps), and text from the coverage mask a [`Typesetter`] produced
//!   (GDI with per-character font fallback on Windows, `win::typeset`; [`BlockType`] in tests).
//!   Alpha is straight, and source-over keeps the picture's transparency where nothing is drawn.
//!   The overlay shows exactly what [`flatten`] exports: both run [`draw`] over the same pixels.

use crate::capture::scale_px;
use crate::frame::{Frame, Rect};
use std::sync::Arc;

/// Whether annotation exists in this build (Step 56: shapes, pen, marker, text).
pub const IMPLEMENTED: bool = true;

/// A colour, straight (not premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    Rgba { r, g, b, a: 255 }
}

/// The colours of the toolbar's colour button, in order (original Noeul choices; the first is
/// the default).
pub const PALETTE: [(&str, Rgba); 8] = [
    ("red", rgb(0xE5, 0x48, 0x4D)),
    ("orange", rgb(0xF7, 0x6B, 0x15)),
    ("yellow", rgb(0xFF, 0xC5, 0x3D)),
    ("green", rgb(0x30, 0xA4, 0x6C)),
    ("blue", rgb(0x00, 0x90, 0xFF)),
    ("purple", rgb(0x8E, 0x4E, 0xC6)),
    ("black", rgb(0x11, 0x11, 0x11)),
    ("white", rgb(0xFF, 0xFF, 0xFF)),
];

/// Line widths in logical pixels, by level (`annotate.width-down`/`-up`, `[`/`]`, the wheel).
pub const WIDTHS: [u32; 7] = [1, 2, 3, 4, 6, 8, 12];
/// The marker's widths by the same levels.
pub const MARKER_WIDTHS: [u32; 7] = [8, 10, 14, 18, 24, 32, 40];
/// Text sizes (logical pixels, the font's em height) by the same levels.
pub const TEXT_SIZES: [u32; 7] = [12, 14, 18, 22, 28, 36, 48];
/// The default level: 3 logical pixels, a 14-pixel marker, 18-pixel text.
pub const DEFAULT_LEVEL: u8 = 2;
/// The marker's opacity (about 40 %): one coverage per stroke, so overlaps do not darken.
pub const MARKER_ALPHA: u8 = 102;
/// Undo steps kept per picture (older ones are dropped).
pub const UNDO_LIMIT: usize = 200;

/// A drawing tool (UX spec §3.1; mosaic, blur, eraser and redaction are Step 57).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Pen,
    Marker,
    Text,
}

impl Tool {
    pub const ALL: [Tool; 7] = [
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Line,
        Tool::Arrow,
        Tool::Pen,
        Tool::Marker,
        Tool::Text,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Rectangle => "rectangle",
            Tool::Ellipse => "ellipse",
            Tool::Line => "line",
            Tool::Arrow => "arrow",
            Tool::Pen => "pen",
            Tool::Marker => "marker",
            Tool::Text => "text",
        }
    }

    /// The hotkey action that chooses it (`annotate.rectangle`, …; bindable, no default key).
    pub fn action_id(self) -> &'static str {
        match self {
            Tool::Rectangle => "annotate.rectangle",
            Tool::Ellipse => "annotate.ellipse",
            Tool::Line => "annotate.line",
            Tool::Arrow => "annotate.arrow",
            Tool::Pen => "annotate.pen",
            Tool::Marker => "annotate.marker",
            Tool::Text => "annotate.text",
        }
    }

    pub fn from_action(id: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.action_id() == id)
    }

    /// Its name for people (toolbar tooltips and the docs).
    pub fn title(self) -> &'static str {
        match self {
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Arrow => "Arrow",
            Tool::Pen => "Pen",
            Tool::Marker => "Marker",
            Tool::Text => "Text",
        }
    }
}

/// A point in device pixels of the picture; pixel `(x, y)` has its centre at `(x + 0.5, y + 0.5)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct P {
    pub x: f32,
    pub y: f32,
}

impl P {
    pub const fn new(x: f32, y: f32) -> P {
        P { x, y }
    }

    /// The centre of pixel `(x, y)`.
    pub fn of_pixel((x, y): (i32, i32)) -> P {
        P::new(x as f32 + 0.5, y as f32 + 0.5)
    }

    fn sub(self, o: P) -> P {
        P::new(self.x - o.x, self.y - o.y)
    }

    fn add(self, o: P) -> P {
        P::new(self.x + o.x, self.y + o.y)
    }

    fn scale(self, k: f32) -> P {
        P::new(self.x * k, self.y * k)
    }

    fn len(self) -> f32 {
        self.x.hypot(self.y)
    }

    fn dot(self, o: P) -> f32 {
        self.x * o.x + self.y * o.y
    }
}

/// An axis-aligned box of pixel edges (`l ≤ r`, `t ≤ b`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Bounds {
    pub fn new(l: f32, t: f32, r: f32, b: f32) -> Bounds {
        Bounds {
            l: l.min(r),
            t: t.min(b),
            r: l.max(r),
            b: t.max(b),
        }
    }

    /// The box of pixels `a` to `b`, both inclusive (a drag from one pixel to another).
    pub fn of_pixels(a: (i32, i32), b: (i32, i32)) -> Bounds {
        Bounds::new(
            a.0.min(b.0) as f32,
            a.1.min(b.1) as f32,
            (a.0.max(b.0) + 1) as f32,
            (a.1.max(b.1) + 1) as f32,
        )
    }

    pub fn of_rect(r: Rect) -> Bounds {
        Bounds::new(r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32)
    }

    fn around(points: &[P]) -> Option<Bounds> {
        let first = points.first()?;
        let mut b = Bounds::new(first.x, first.y, first.x, first.y);
        for p in points {
            b = b.union(Bounds::new(p.x, p.y, p.x, p.y));
        }
        Some(b)
    }

    pub fn width(&self) -> f32 {
        self.r - self.l
    }

    pub fn height(&self) -> f32 {
        self.b - self.t
    }

    pub fn union(self, o: Bounds) -> Bounds {
        Bounds {
            l: self.l.min(o.l),
            t: self.t.min(o.t),
            r: self.r.max(o.r),
            b: self.b.max(o.b),
        }
    }

    pub fn grow(self, by: f32) -> Bounds {
        Bounds::new(self.l - by, self.t - by, self.r + by, self.b + by)
    }

    fn contains(&self, p: P) -> bool {
        p.x >= self.l && p.x <= self.r && p.y >= self.t && p.y <= self.b
    }

    fn translate(self, dx: f32, dy: f32) -> Bounds {
        Bounds::new(self.l + dx, self.t + dy, self.r + dx, self.b + dy)
    }

    /// The whole pixels this box touches.
    pub fn to_rect(self) -> Option<Rect> {
        let clampi = |v: f32| v.clamp(-2.0e9, 2.0e9) as i64;
        let (l, t) = (clampi(self.l.floor()), clampi(self.t.floor()));
        let (r, b) = (clampi(self.r.ceil()), clampi(self.b.ceil()));
        if r <= l || b <= t {
            return None;
        }
        Some(Rect::new(
            i32::try_from(l).ok()?,
            i32::try_from(t).ok()?,
            u32::try_from(r - l).ok()?,
            u32::try_from(b - t).ok()?,
        ))
    }
}

/// A text label's laid-out pixels: a coverage mask (0 = nothing, 255 = full ink) of the whole
/// label, top-left at the label's position, and where the caret goes before each character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyphs {
    /// The text and size these glyphs were set for.
    pub text: String,
    pub size: u32,
    pub width: u32,
    pub height: u32,
    pub mask: Vec<u8>,
    /// The height of one line (lines are separated by `\n`).
    pub line_height: u32,
    /// `carets[line][i]` is the x offset before the `i`-th character of that line; one more entry
    /// than the line has characters (the end).
    pub carets: Vec<Vec<u32>>,
}

/// Lays text out into a coverage mask (GDI with font fallback on Windows; [`BlockType`] here).
pub trait Typesetter {
    /// `text` (lines separated by `\n`) at `size` device pixels (the em height).
    fn typeset(&self, text: &str, size: u32) -> Glyphs;
}

/// A stand-in typesetter for tests and platforms without one: every character a solid block
/// (0.6 em wide, the ink from 0.25 to 0.85 em), spaces blank, lines 1.25 em apart. Deterministic,
/// so text pixels can be checked exactly.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlockType;

impl Typesetter for BlockType {
    fn typeset(&self, text: &str, size: u32) -> Glyphs {
        let size = size.max(4);
        let adv = (size * 3).div_ceil(5);
        let lh = (size * 5).div_ceil(4);
        let lines: Vec<&str> = text.split('\n').collect();
        let carets: Vec<Vec<u32>> = lines
            .iter()
            .map(|l| (0..=l.chars().count()).map(|i| i as u32 * adv).collect())
            .collect();
        let width = carets
            .iter()
            .map(|c| c.last().copied().unwrap_or(0))
            .max()
            .unwrap_or(0)
            .max(1);
        let height = lh * u32::try_from(lines.len()).unwrap_or(1).max(1);
        let mut mask = vec![0u8; (width * height) as usize];
        let (top, bottom) = (size / 4, size * 17 / 20);
        for (li, line) in lines.iter().enumerate() {
            for (ci, ch) in line.chars().enumerate() {
                if ch.is_whitespace() {
                    continue;
                }
                let x0 = ci as u32 * adv;
                for y in li as u32 * lh + top..li as u32 * lh + bottom {
                    for x in x0..x0 + adv.saturating_sub(1).max(1) {
                        if x < width && y < height {
                            mask[(y * width + x) as usize] = 255;
                        }
                    }
                }
            }
        }
        Glyphs {
            text: text.to_string(),
            size,
            width,
            height,
            mask,
            line_height: lh,
            carets,
        }
    }
}

/// A text label: its top-left corner, its text, and the glyphs once typeset.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub at: (i32, i32),
    pub text: String,
    pub glyphs: Option<Arc<Glyphs>>,
}

/// What an object is.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// An outline whose stroke lies inside the box.
    Rectangle(Bounds),
    Ellipse(Bounds),
    Line(P, P),
    /// From the first point to the head at the second.
    Arrow(P, P),
    Pen(Vec<P>),
    Marker(Vec<P>),
    Text(Text),
}

impl Shape {
    pub fn kind(&self) -> Tool {
        match self {
            Shape::Rectangle(_) => Tool::Rectangle,
            Shape::Ellipse(_) => Tool::Ellipse,
            Shape::Line(..) => Tool::Line,
            Shape::Arrow(..) => Tool::Arrow,
            Shape::Pen(_) => Tool::Pen,
            Shape::Marker(_) => Tool::Marker,
            Shape::Text(_) => Tool::Text,
        }
    }
}

/// One annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct Object {
    /// Stable within its picture (selection, undo); never reused there.
    pub id: u32,
    pub shape: Shape,
    pub color: Rgba,
    /// Index into [`WIDTHS`] (and [`MARKER_WIDTHS`], [`TEXT_SIZES`]).
    pub level: u8,
    /// The DPI of the monitor it was drawn on: its widths are logical pixels at that DPI.
    pub dpi: u32,
}

fn level_of(level: u8) -> usize {
    usize::from(level).min(WIDTHS.len() - 1)
}

/// The stroke width in device pixels of `kind` at `level` and `dpi`.
pub fn stroke_px(kind: Tool, level: u8, dpi: u32) -> u32 {
    let i = level_of(level);
    match kind {
        Tool::Marker => scale_px(MARKER_WIDTHS[i], dpi),
        Tool::Text => scale_px(TEXT_SIZES[i], dpi),
        _ => scale_px(WIDTHS[i], dpi),
    }
}

/// The arrow head's length and half width for a shaft `w` device pixels wide.
pub fn arrow_head(w: f32) -> (f32, f32) {
    let len = 4.0 * w + 4.0;
    (len, len * 0.45)
}

impl Object {
    /// The stroke width (or text size) in device pixels.
    pub fn width(&self) -> f32 {
        stroke_px(self.shape.kind(), self.level, self.dpi) as f32
    }

    pub fn text_size(&self) -> u32 {
        stroke_px(Tool::Text, self.level, self.dpi)
    }

    /// Every pixel it may touch (anti-aliasing included).
    pub fn bounds(&self) -> Bounds {
        let w = self.width();
        match &self.shape {
            Shape::Rectangle(b) | Shape::Ellipse(b) => b.grow(1.0),
            Shape::Line(a, b) => Bounds::new(a.x, a.y, b.x, b.y).grow(w / 2.0 + 1.0),
            Shape::Arrow(a, b) => {
                let (_, half) = arrow_head(w);
                Bounds::new(a.x, a.y, b.x, b.y).grow(half.max(w / 2.0) + 1.0)
            }
            Shape::Pen(pts) | Shape::Marker(pts) => Bounds::around(pts)
                .unwrap_or(Bounds::new(0.0, 0.0, 0.0, 0.0))
                .grow(w / 2.0 + 1.0),
            Shape::Text(t) => {
                let (gw, gh) = text_extent(t, self.text_size());
                Bounds::new(
                    t.at.0 as f32,
                    t.at.1 as f32,
                    t.at.0 as f32 + gw as f32,
                    t.at.1 as f32 + gh as f32,
                )
            }
        }
    }

    /// Whether a press at `p` picks this object: on its stroke (within `tol` device pixels), or
    /// anywhere on a text label.
    pub fn hit(&self, p: P, tol: f32) -> bool {
        let half = self.width() / 2.0 + tol;
        match &self.shape {
            Shape::Rectangle(b) => {
                let inner = b.grow(-(self.width() + tol));
                b.grow(tol).contains(p) && !(inner.width() > 0.0 && inner.contains(p))
            }
            Shape::Ellipse(b) => {
                let w = self.width();
                let (c, ra, rb) = ellipse_axes(*b, w);
                if ra <= 0.5 || rb <= 0.5 {
                    return b.grow(tol).contains(p);
                }
                ellipse_distance(p.sub(c), ra, rb).abs() <= half
            }
            Shape::Line(a, b) => segment_distance(p, *a, *b) <= half,
            Shape::Arrow(a, b) => {
                segment_distance(p, *a, *b) <= half.max(arrow_head(self.width()).1 * 0.6)
            }
            Shape::Pen(pts) | Shape::Marker(pts) => polyline_distance(p, pts) <= half,
            Shape::Text(_) => self.bounds().grow(tol).contains(p),
        }
    }

    /// The same object moved by whole device pixels.
    #[must_use]
    pub fn translated(&self, dx: i32, dy: i32) -> Object {
        let (fx, fy) = (dx as f32, dy as f32);
        let mv = |p: &P| P::new(p.x + fx, p.y + fy);
        let shape = match &self.shape {
            Shape::Rectangle(b) => Shape::Rectangle(b.translate(fx, fy)),
            Shape::Ellipse(b) => Shape::Ellipse(b.translate(fx, fy)),
            Shape::Line(a, b) => Shape::Line(mv(a), mv(b)),
            Shape::Arrow(a, b) => Shape::Arrow(mv(a), mv(b)),
            Shape::Pen(pts) => Shape::Pen(pts.iter().map(mv).collect()),
            Shape::Marker(pts) => Shape::Marker(pts.iter().map(mv).collect()),
            Shape::Text(t) => Shape::Text(Text {
                at: (t.at.0.saturating_add(dx), t.at.1.saturating_add(dy)),
                text: t.text.clone(),
                glyphs: t.glyphs.clone(),
            }),
        };
        Object {
            shape,
            ..self.clone()
        }
    }

    /// What the size readout says about it while it is drawn or selected (NOEUL-FR-083): the
    /// box of a shape, the length and angle of a line.
    pub fn measure(&self) -> String {
        match &self.shape {
            Shape::Rectangle(b) | Shape::Ellipse(b) => {
                format!("{} \u{d7} {}", b.width().round(), b.height().round())
            }
            Shape::Line(a, b) | Shape::Arrow(a, b) => {
                let d = b.sub(*a);
                let deg = (-d.y).atan2(d.x).to_degrees();
                format!("{:.0} px  {:.1}\u{b0}", d.len(), deg)
            }
            Shape::Pen(_) | Shape::Marker(_) | Shape::Text(_) => {
                let b = self.bounds();
                format!("{} \u{d7} {}", b.width().round(), b.height().round())
            }
        }
    }
}

/// The size of a label: its glyphs, or an estimate until it is typeset.
fn text_extent(t: &Text, size: u32) -> (u32, u32) {
    match &t.glyphs {
        Some(g) => (g.width.max(1), g.height.max(1)),
        None => {
            let lines: Vec<&str> = t.text.split('\n').collect();
            let w = lines
                .iter()
                .map(|l| l.chars().count() as u32)
                .max()
                .unwrap_or(0);
            (
                (w * size * 3 / 5).max(1),
                (size * 5 / 4) * lines.len() as u32,
            )
        }
    }
}

/// The objects of one picture and their undo history.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    objects: Vec<Object>,
    undo: Vec<Vec<Object>>,
    redo: Vec<Vec<Object>>,
    next: u32,
}

impl Doc {
    pub fn new() -> Doc {
        Doc::default()
    }

    /// A document holding `objects` (a pin's committed annotations), with no history.
    pub fn with(objects: Vec<Object>) -> Doc {
        let next = objects.iter().map(|o| o.id).max().unwrap_or(0);
        Doc {
            objects,
            undo: Vec::new(),
            redo: Vec::new(),
            next,
        }
    }

    /// The objects in drawing order (the last on top).
    pub fn objects(&self) -> &[Object] {
        &self.objects
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn undo_steps(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_steps(&self) -> usize {
        self.redo.len()
    }

    pub fn get(&self, id: u32) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.objects.iter().position(|o| o.id == id)
    }

    fn new_id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    /// Records the state `before` an edit as one undo step (nothing when the edit changed
    /// nothing) and forgets the redo steps.
    fn record(&mut self, before: Vec<Object>) {
        if before == self.objects {
            return;
        }
        self.undo.push(before);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Undoes the last edit; `false` when there is none.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        let now = std::mem::replace(&mut self.objects, prev);
        self.redo.push(now);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let now = std::mem::replace(&mut self.objects, next);
        self.undo.push(now);
        true
    }

    /// Adds an object on top as one step; returns its ID.
    pub fn add(&mut self, mut o: Object) -> u32 {
        let before = self.objects.clone();
        o.id = self.new_id();
        let id = o.id;
        self.objects.push(o);
        self.record(before);
        id
    }

    /// Changes one object as one step.
    pub fn modify(&mut self, id: u32, f: impl FnOnce(&mut Object)) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let before = self.objects.clone();
        f(&mut self.objects[i]);
        self.record(before);
        true
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let before = self.objects.clone();
        self.objects.remove(i);
        self.record(before);
        true
    }

    /// Removes every object as one undoable step (`annotate.clear-all`).
    pub fn clear(&mut self) -> bool {
        if self.objects.is_empty() {
            return false;
        }
        let before = std::mem::take(&mut self.objects);
        self.record(before);
        true
    }

    /// Moves an object in the drawing order as one step; `false` when it is already there.
    pub fn reorder(&mut self, id: u32, order: Order) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let last = self.objects.len() - 1;
        let to = match order {
            Order::Forward => (i + 1).min(last),
            Order::Backward => i.saturating_sub(1),
            Order::Front => last,
            Order::Back => 0,
        };
        if to == i {
            return false;
        }
        let before = self.objects.clone();
        let o = self.objects.remove(i);
        self.objects.insert(to, o);
        self.record(before);
        true
    }

    /// The topmost object a press at `p` picks.
    pub fn hit(&self, p: P, tol: f32) -> Option<u32> {
        self.objects
            .iter()
            .rev()
            .find(|o| o.hit(p, tol))
            .map(|o| o.id)
    }

    /// Every pixel some object may touch.
    pub fn bounds(&self) -> Option<Bounds> {
        self.objects
            .iter()
            .map(Object::bounds)
            .reduce(Bounds::union)
    }
}

/// A change of the drawing order (`annotate.bring-forward`, `-send-backward`, `-to-front`,
/// `-to-back`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Forward,
    Backward,
    Front,
    Back,
}

/// A handle of the selected object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// A corner or edge of the box: which edges it moves.
    Box {
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
    },
    /// The first or second end of a line or arrow.
    Start,
    End,
}

/// A key while a text label is edited (every other key is suspended, UX spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKey {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// A new line (`Enter`).
    Newline,
}

/// What the input method reports (`WM_IME_*`, NOEUL-NFR-014).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ime {
    /// A composition started.
    Start,
    /// The text being composed now (shown at the caret, underlined, not yet part of the label).
    Compose(String),
    /// Text the user confirmed: it replaces the composition.
    Commit(String),
    /// The composition ended (a composition still shown is dropped).
    End,
}

#[derive(Debug, Clone, PartialEq)]
enum Gesture {
    /// A new object being drawn from `from`; it is in the document already, recorded as one step
    /// when the gesture ends.
    Draw { id: u32, from: (i32, i32) },
    Move {
        id: u32,
        grab: (i32, i32),
        start: Object,
    },
    Resize {
        id: u32,
        handle: Handle,
        start: Object,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct TextEdit {
    id: u32,
    /// The caret, in characters.
    caret: usize,
    /// The other end of a selected range, if any.
    anchor: Option<usize>,
    /// The composition shown at the caret: its start and length in characters.
    comp: Option<(usize, usize)>,
}

/// The annotation editor of one picture: the document, the current tool and style, and what the
/// pointer or keyboard is doing. See the module documentation.
#[derive(Debug, Clone)]
pub struct Editor {
    doc: Doc,
    tool: Option<Tool>,
    color: usize,
    level: u8,
    selected: Option<u32>,
    gesture: Option<Gesture>,
    /// The state before the running gesture or text edit (one undo step when it ends).
    before: Option<Vec<Object>>,
    /// The gesture is driven by `selection.mark` and the pointer keys.
    keyboard: bool,
    text: Option<TextEdit>,
    /// Where drawing is allowed (the selection, or the pin's picture).
    clip: Bounds,
    dirty: Option<Bounds>,
    /// A label changed and must be typeset again.
    retype: bool,
}

impl Default for Editor {
    fn default() -> Editor {
        Editor::new(Doc::new())
    }
}

/// Snaps `to` so the line from `from` is horizontal, vertical or diagonal (`Shift`).
fn snap45(from: P, to: P) -> P {
    let d = to.sub(from);
    let len = d.len();
    if len == 0.0 {
        return to;
    }
    let ang = d.y.atan2(d.x);
    let step = std::f32::consts::FRAC_PI_4;
    let a = (ang / step).round() * step;
    P::new(from.x + len * a.cos(), from.y + len * a.sin())
}

impl Editor {
    pub fn new(doc: Doc) -> Editor {
        Editor {
            doc,
            tool: None,
            color: 0,
            level: DEFAULT_LEVEL,
            selected: None,
            gesture: None,
            before: None,
            keyboard: false,
            text: None,
            clip: Bounds::new(-1.0e9, -1.0e9, 1.0e9, 1.0e9),
            dirty: None,
            retype: false,
        }
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The objects as they are now (an output takes these).
    pub fn objects(&self) -> &[Object] {
        self.doc.objects()
    }

    pub fn tool(&self) -> Option<Tool> {
        self.tool
    }

    pub fn color(&self) -> (&'static str, Rgba) {
        PALETTE[self.color % PALETTE.len()]
    }

    pub fn level(&self) -> u8 {
        self.level
    }

    pub fn selected(&self) -> Option<u32> {
        self.selected.filter(|id| self.doc.get(*id).is_some())
    }

    pub fn editing_text(&self) -> bool {
        self.text.is_some()
    }

    pub fn composing(&self) -> bool {
        self.text.as_ref().is_some_and(|t| t.comp.is_some())
    }

    /// Whether a press, drag or keyboard gesture is running.
    pub fn busy(&self) -> bool {
        self.gesture.is_some()
    }

    /// Whether a keyboard gesture (started by `selection.mark`) is waiting for its end point.
    pub fn marking(&self) -> bool {
        self.gesture.is_some() && self.keyboard
    }

    /// Where drawing is allowed: points are kept inside it.
    pub fn set_clip(&mut self, clip: Rect) {
        self.clip = Bounds::of_rect(clip);
    }

    fn clamp(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let c = self.clip;
        let cx = (x as f32).clamp(c.l, (c.r - 1.0).max(c.l)) as i32;
        let cy = (y as f32).clamp(c.t, (c.b - 1.0).max(c.t)) as i32;
        (cx, cy)
    }

    fn mark_dirty(&mut self, b: Bounds) {
        self.dirty = Some(self.dirty.map_or(b, |d| d.union(b)));
    }

    fn mark_object(&mut self, id: u32) {
        if let Some(b) = self.doc.get(id).map(Object::bounds) {
            self.mark_dirty(b);
        }
    }

    fn mark_all(&mut self) {
        if let Some(b) = self.doc.bounds() {
            self.mark_dirty(b);
        }
    }

    /// The pixels that changed since the last call (the overlay redraws them).
    pub fn take_dirty(&mut self) -> Option<Rect> {
        self.dirty.take().and_then(Bounds::to_rect)
    }

    /// Chooses a tool (`None`: no tool, presses act on the selection again). Choosing the
    /// current tool again leaves it, as its toolbar button toggles.
    pub fn set_tool(&mut self, tool: Option<Tool>) {
        self.finish();
        self.tool = tool;
        if tool.is_none() {
            self.deselect();
        }
    }

    /// `annotate.done` (`Escape`): ends a text edit, else aborts a gesture, else leaves the tool
    /// and drops the selection. `false` when there was nothing to end (the key then belongs to
    /// the outer context: `Escape` cancels the capture).
    pub fn done(&mut self) -> bool {
        if self.text.is_some() {
            self.end_text();
            return true;
        }
        if self.gesture.is_some() {
            self.abort();
            return true;
        }
        if self.tool.is_some() || self.selected().is_some() {
            self.tool = None;
            self.deselect();
            return true;
        }
        false
    }

    fn deselect(&mut self) {
        if let Some(id) = self.selected.take() {
            self.mark_object(id);
        }
    }

    fn select(&mut self, id: u32) {
        self.deselect();
        self.selected = Some(id);
        self.mark_object(id);
    }

    /// Ends whatever runs: a gesture is kept as drawn, a text edit is committed.
    pub fn finish(&mut self) {
        if self.gesture.is_some() {
            self.end_gesture();
        }
        if self.text.is_some() {
            self.end_text();
        }
    }

    /// The colour as an index into [`PALETTE`]; applies to the selected object too.
    pub fn set_color(&mut self, index: usize) {
        self.color = index % PALETTE.len();
        let c = PALETTE[self.color].1;
        if let Some(id) = self.selected().filter(|_| self.gesture.is_none()) {
            self.doc.modify(id, |o| o.color = c);
            self.mark_object(id);
        }
    }

    /// The next (or previous) colour of the palette.
    pub fn step_color(&mut self, forward: bool) {
        let n = PALETTE.len();
        self.set_color(if forward {
            self.color + 1
        } else {
            self.color + n - 1
        });
    }

    /// One width level thinner or thicker (`[`, `]`, the wheel); applies to the selected object.
    pub fn step_width(&mut self, up: bool) -> bool {
        let max = (WIDTHS.len() - 1) as u8;
        let next = if up {
            self.level.saturating_add(1).min(max)
        } else {
            self.level.saturating_sub(1)
        };
        let changed = next != self.level;
        self.level = next;
        if let Some(id) = self.selected().filter(|_| self.gesture.is_none()) {
            self.mark_object(id);
            self.doc.modify(id, |o| o.level = next);
            self.mark_object(id);
            self.retype = true;
            return true;
        }
        changed
    }

    fn new_object(&self, shape: Shape, dpi: u32) -> Object {
        Object {
            id: 0,
            shape,
            color: PALETTE[self.color].1,
            level: self.level,
            dpi,
        }
    }

    /// A left press at pixel `at` with the current tool, on a monitor at `dpi`. Returns `false`
    /// when there is no tool and nothing was picked (the press belongs to the selection then).
    pub fn press(&mut self, at: (i32, i32), dpi: u32) -> bool {
        self.keyboard = false;
        let tol = scale_px(4, dpi) as f32;
        let p = P::of_pixel(at);
        // A press on a handle of the selected object resizes it; on an object, picks it.
        if let Some((id, h)) = self.handle_at(at, dpi) {
            self.end_text_unless(id);
            self.begin(Gesture::Resize {
                id,
                handle: h,
                start: self.doc.get(id).cloned().expect("selected"),
            });
            return true;
        }
        if let Some(t) = &self.text {
            // Inside the label being edited: the caret goes there.
            let id = t.id;
            if self.doc.get(id).is_some_and(|o| o.hit(p, tol)) {
                self.caret_at(at);
                return true;
            }
            self.end_text();
            if self.tool == Some(Tool::Text) {
                // A press outside the label ends it; the next press starts a new one.
                return true;
            }
        }
        // Freehand tools always draw (a stroke may start on another one); the other tools, and
        // no tool, pick the object under the press.
        let picks = !matches!(self.tool, Some(Tool::Pen | Tool::Marker));
        if let Some(id) = self.doc.hit(p, tol).filter(|_| picks) {
            if self.tool == Some(Tool::Text)
                && matches!(self.doc.get(id).map(|o| &o.shape), Some(Shape::Text(_)))
            {
                // The text tool edits the label it presses on, the caret where it pressed.
                self.edit_text(id);
                self.caret_at(at);
                return true;
            }
            self.select(id);
            let start = self.doc.get(id).cloned().expect("hit");
            self.begin(Gesture::Move {
                id,
                grab: at,
                start,
            });
            return true;
        }
        self.deselect();
        let Some(tool) = self.tool else {
            return false;
        };
        if tool == Tool::Text {
            self.start_text(self.clamp(at), dpi);
            return true;
        }
        self.start_draw(tool, at, dpi)
    }

    fn begin(&mut self, g: Gesture) {
        self.before = Some(self.doc.objects.clone());
        self.gesture = Some(g);
    }

    /// The pointer moved to `at` while the button is held (or, in a keyboard gesture, the
    /// pointer keys moved it). `shift` squares a shape and snaps a line to 45°.
    pub fn drag(&mut self, at: (i32, i32), shift: bool) {
        let Some(g) = self.gesture.clone() else {
            return;
        };
        let at = self.clamp(at);
        match g {
            Gesture::Draw { id, from } => {
                self.mark_object(id);
                let (fp, tp) = (P::of_pixel(from), P::of_pixel(at));
                if let Some(i) = self.doc.index(id) {
                    let o = &mut self.doc.objects[i];
                    match &mut o.shape {
                        Shape::Rectangle(b) | Shape::Ellipse(b) => {
                            let mut to = at;
                            if shift {
                                let d = (at.0 - from.0).abs().max((at.1 - from.1).abs());
                                to = (
                                    from.0 + d * (at.0 - from.0).signum(),
                                    from.1 + d * (at.1 - from.1).signum(),
                                );
                            }
                            *b = Bounds::of_pixels(from, to);
                        }
                        Shape::Line(_, b) | Shape::Arrow(_, b) => {
                            *b = if shift { snap45(fp, tp) } else { tp };
                        }
                        Shape::Pen(pts) | Shape::Marker(pts) => {
                            if pts.last() != Some(&tp) {
                                pts.push(tp);
                            }
                        }
                        Shape::Text(_) => {}
                    }
                }
                self.mark_object(id);
            }
            Gesture::Move { id, grab, start } => {
                self.mark_object(id);
                let moved = start.translated(at.0 - grab.0, at.1 - grab.1);
                if let Some(i) = self.doc.index(id) {
                    self.doc.objects[i] = moved;
                }
                self.mark_object(id);
            }
            Gesture::Resize { id, handle, start } => {
                self.mark_object(id);
                let resized = resized(&start, handle, at);
                if let Some(i) = self.doc.index(id) {
                    self.doc.objects[i] = resized;
                }
                self.mark_object(id);
            }
        }
    }

    /// The button was released at `at`: the gesture ends as one undo step.
    pub fn release(&mut self, at: (i32, i32), shift: bool) {
        if self.keyboard {
            return;
        }
        if self.gesture.is_some() {
            self.drag(at, shift);
            self.end_gesture();
        }
    }

    fn end_gesture(&mut self) {
        let Some(g) = self.gesture.take() else {
            return;
        };
        self.keyboard = false;
        let before = self.before.take().unwrap_or_default();
        if let Gesture::Draw { id, .. } = g {
            // A click without a drag draws nothing for a box or a line (a dot for the pen).
            let keep = self.doc.get(id).is_some_and(|o| match &o.shape {
                Shape::Rectangle(b) | Shape::Ellipse(b) => b.width() > 1.0 || b.height() > 1.0,
                Shape::Line(a, b) | Shape::Arrow(a, b) => a != b,
                _ => true,
            });
            if !keep {
                self.mark_object(id);
                self.doc.objects.retain(|o| o.id != id);
            }
        }
        self.doc.record(before);
    }

    /// Ends the gesture without its change (`Escape` while drawing, a lost pointer capture).
    pub fn abort(&mut self) {
        if self.gesture.take().is_some() {
            self.keyboard = false;
            self.mark_all();
            if let Some(b) = self.before.take() {
                self.doc.objects = b;
            }
            self.mark_all();
        }
    }

    /// A right-click: finishes the current shape (ANN-10); `false` when nothing ran.
    pub fn right_click(&mut self) -> bool {
        if self.gesture.is_some() {
            self.end_gesture();
            return true;
        }
        false
    }

    /// `selection.mark` (`Space`) with a tool: the first press sets the start of a shape at the
    /// pointer (a label starts there with the text tool), the second its end; the pointer keys
    /// move the end in between (keyboard-only drawing, NOEUL-NFR-012).
    pub fn mark(&mut self, at: (i32, i32), dpi: u32, shift: bool) -> bool {
        if self.gesture.is_some() && self.keyboard {
            self.drag(at, shift);
            self.end_gesture();
            return true;
        }
        let Some(tool) = self.tool else {
            return false;
        };
        self.end_text();
        if tool == Tool::Text {
            self.start_text(self.clamp(at), dpi);
            return true;
        }
        // A mark always starts a new shape, even over another object.
        self.deselect();
        let ok = self.start_draw(tool, at, dpi);
        self.keyboard = ok;
        ok
    }

    fn start_draw(&mut self, tool: Tool, at: (i32, i32), dpi: u32) -> bool {
        let at = self.clamp(at);
        let p = P::of_pixel(at);
        let shape = match tool {
            Tool::Rectangle => Shape::Rectangle(Bounds::of_pixels(at, at)),
            Tool::Ellipse => Shape::Ellipse(Bounds::of_pixels(at, at)),
            Tool::Line => Shape::Line(p, p),
            Tool::Arrow => Shape::Arrow(p, p),
            Tool::Pen => Shape::Pen(vec![p]),
            Tool::Marker => Shape::Marker(vec![p]),
            Tool::Text => return false,
        };
        let before = self.doc.objects.clone();
        let mut o = self.new_object(shape, dpi);
        o.id = self.doc.new_id();
        let id = o.id;
        self.doc.objects.push(o);
        self.before = Some(before);
        self.gesture = Some(Gesture::Draw { id, from: at });
        self.mark_object(id);
        true
    }

    /// The handle of the selected object under `at`, if any (9 logical pixels around each).
    pub fn handle_at(&self, at: (i32, i32), dpi: u32) -> Option<(u32, Handle)> {
        let id = self.selected()?;
        if self.text.as_ref().is_some_and(|t| t.id == id) {
            return None;
        }
        let half = (scale_px(9, dpi) / 2) as f32 + 0.5;
        let p = P::of_pixel(at);
        handles(self.doc.get(id)?)
            .into_iter()
            .find(|(_, c)| (p.x - c.x).abs() <= half && (p.y - c.y).abs() <= half)
            .map(|(h, _)| (id, h))
    }

    /// The handles of the selected object and where they are (none while it is typed into).
    pub fn selected_handles(&self) -> Vec<(Handle, P)> {
        match self.selected() {
            Some(id) if !self.text.as_ref().is_some_and(|t| t.id == id) => {
                self.doc.get(id).map(handles).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// The box around the selected object (drawn as a dashed outline).
    pub fn selected_bounds(&self) -> Option<Bounds> {
        self.selected()
            .and_then(|id| self.doc.get(id))
            .map(Object::bounds)
    }

    /// The readout of the object drawn or selected now (NOEUL-FR-083).
    pub fn measure(&self) -> Option<String> {
        let id = match &self.gesture {
            Some(
                Gesture::Draw { id, .. } | Gesture::Move { id, .. } | Gesture::Resize { id, .. },
            ) => Some(*id),
            None => self.selected(),
        }?;
        self.doc.get(id).map(Object::measure)
    }

    /// Undo (`annotate.undo`): the last edit; a running gesture or text edit ends first.
    pub fn undo(&mut self) -> bool {
        self.abort();
        if self.text.is_some() {
            self.end_text();
        }
        self.mark_all();
        let ok = self.doc.undo();
        self.mark_all();
        self.retype = true;
        ok
    }

    pub fn redo(&mut self) -> bool {
        self.abort();
        if self.text.is_some() {
            self.end_text();
        }
        self.mark_all();
        let ok = self.doc.redo();
        self.mark_all();
        self.retype = true;
        ok
    }

    /// `annotate.clear-all`: every object, as one undoable step.
    pub fn clear_all(&mut self) -> bool {
        self.finish();
        self.mark_all();
        self.selected = None;
        self.doc.clear()
    }

    /// `annotate.delete` (`Delete`): the selected object.
    pub fn delete_selected(&mut self) -> bool {
        let Some(id) = self.selected().filter(|_| self.gesture.is_none()) else {
            return false;
        };
        self.mark_object(id);
        self.selected = None;
        self.doc.remove(id)
    }

    /// `annotate.bring-forward` and the like: the selected object.
    pub fn reorder(&mut self, order: Order) -> bool {
        let Some(id) = self.selected().filter(|_| self.gesture.is_none()) else {
            return false;
        };
        self.mark_all();
        self.doc.reorder(id, order)
    }

    /// `annotate.select-next` / `-previous`: the next object in drawing order (keyboard editing).
    pub fn select_step(&mut self, forward: bool) -> bool {
        if self.gesture.is_some() {
            return false;
        }
        self.end_text();
        let n = self.doc.len();
        if n == 0 {
            return false;
        }
        let i = self.selected().and_then(|id| self.doc.index(id)).map_or(
            if forward { 0 } else { n - 1 },
            |i| {
                if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                }
            },
        );
        let id = self.doc.objects[i].id;
        self.select(id);
        true
    }

    /// Moves the selected object by whole pixels (the selection keys act on it while one is
    /// selected), as one step.
    pub fn nudge(&mut self, dx: i32, dy: i32) -> bool {
        let Some(id) = self.selected().filter(|_| self.gesture.is_none()) else {
            return false;
        };
        self.mark_object(id);
        let moved = self.doc.get(id).map(|o| o.translated(dx, dy));
        if let Some(m) = moved {
            self.doc.modify(id, |o| *o = m);
        }
        self.mark_object(id);
        true
    }

    /// Grows (`grow`) or shrinks the selected object towards a direction by `n` pixels
    /// (`Ctrl`/`Shift`+arrows while it is selected): a box edge; a line's end (grow) or start
    /// (shrink); a label's size level.
    pub fn resize_by(&mut self, dir: (i32, i32), n: i32, grow: bool) -> bool {
        let Some(id) = self.selected().filter(|_| self.gesture.is_none()) else {
            return false;
        };
        let Some(o) = self.doc.get(id).cloned() else {
            return false;
        };
        self.mark_object(id);
        let next = match &o.shape {
            Shape::Text(_) => {
                let max = (WIDTHS.len() - 1) as u8;
                let level = if grow {
                    o.level.saturating_add(1).min(max)
                } else {
                    o.level.saturating_sub(1)
                };
                self.retype = true;
                Object { level, ..o.clone() }
            }
            Shape::Line(a, b) | Shape::Arrow(a, b) => {
                let d = P::new((dir.0 * n) as f32, (dir.1 * n) as f32);
                let (a, b) = if grow { (*a, b.add(d)) } else { (a.add(d), *b) };
                Object {
                    shape: if matches!(o.shape, Shape::Line(..)) {
                        Shape::Line(a, b)
                    } else {
                        Shape::Arrow(a, b)
                    },
                    ..o.clone()
                }
            }
            _ => {
                let b = o.bounds_of_shape();
                let k = n as f32;
                let mut nb = b;
                match (dir, grow) {
                    ((0, -1), true) => nb.t -= k,
                    ((0, 1), true) => nb.b += k,
                    ((-1, 0), true) => nb.l -= k,
                    ((1, 0), true) => nb.r += k,
                    // Shrinking moves the opposite edge, as the selection's Shift+arrows do.
                    ((0, -1), false) => nb.b = (nb.b - k).max(nb.t + 1.0),
                    ((0, 1), false) => nb.t = (nb.t + k).min(nb.b - 1.0),
                    ((-1, 0), false) => nb.r = (nb.r - k).max(nb.l + 1.0),
                    ((1, 0), false) => nb.l = (nb.l + k).min(nb.r - 1.0),
                    _ => {}
                }
                reshaped(&o, b, nb)
            }
        };
        self.doc.modify(id, |x| *x = next);
        self.mark_object(id);
        true
    }

    // ----- text labels -----

    fn start_text(&mut self, at: (i32, i32), dpi: u32) {
        self.deselect();
        let before = self.doc.objects.clone();
        let mut o = self.new_object(
            Shape::Text(Text {
                at,
                text: String::new(),
                glyphs: None,
            }),
            dpi,
        );
        o.id = self.doc.new_id();
        let id = o.id;
        self.doc.objects.push(o);
        self.before = Some(before);
        self.selected = Some(id);
        self.text = Some(TextEdit {
            id,
            caret: 0,
            anchor: None,
            comp: None,
        });
        self.retype = true;
        self.mark_object(id);
    }

    /// Starts editing an existing label (a press on it with the text tool, or `Enter` on it).
    pub fn edit_text(&mut self, id: u32) -> bool {
        let Some(Shape::Text(t)) = self.doc.get(id).map(|o| &o.shape) else {
            return false;
        };
        let n = t.text.chars().count();
        self.end_text();
        self.before = Some(self.doc.objects.clone());
        self.selected = Some(id);
        self.text = Some(TextEdit {
            id,
            caret: n,
            anchor: None,
            comp: None,
        });
        self.mark_object(id);
        true
    }

    fn end_text_unless(&mut self, id: u32) {
        if self.text.as_ref().is_some_and(|t| t.id != id) {
            self.end_text();
        }
    }

    /// Ends editing the label: an empty label is removed, the label is deselected, and the whole
    /// edit is one undo step.
    pub fn end_text(&mut self) {
        let Some(t) = self.text.take() else {
            return;
        };
        self.mark_object(t.id);
        if let Some((start, len)) = t.comp {
            self.splice(t.id, start, len, "");
        }
        let empty = matches!(self.doc.get(t.id).map(|o| &o.shape),
            Some(Shape::Text(x)) if x.text.is_empty());
        if empty {
            self.doc.objects.retain(|o| o.id != t.id);
        }
        // A finished label is not selected (the style keys then change the style only).
        if self.selected == Some(t.id) {
            self.selected = None;
        }
        let before = self.before.take().unwrap_or_default();
        self.doc.record(before);
    }

    fn text_of(&self, id: u32) -> Option<&Text> {
        match self.doc.get(id).map(|o| &o.shape) {
            Some(Shape::Text(t)) => Some(t),
            _ => None,
        }
    }

    /// Replaces `len` characters at `start` of label `id` with `with`.
    fn splice(&mut self, id: u32, start: usize, len: usize, with: &str) {
        self.mark_object(id);
        if let Some(i) = self.doc.index(id)
            && let Shape::Text(t) = &mut self.doc.objects[i].shape
        {
            let chars: Vec<char> = t.text.chars().collect();
            let s = start.min(chars.len());
            let e = (start + len).min(chars.len());
            let mut out: String = chars[..s].iter().collect();
            out.push_str(with);
            out.extend(&chars[e..]);
            t.text = out;
            t.glyphs = None;
        }
        self.retype = true;
        self.mark_object(id);
    }

    /// Deletes the selected range of the edited label; `true` when there was one.
    fn delete_range(&mut self) -> bool {
        let Some(t) = self.text.clone() else {
            return false;
        };
        let Some(a) = t.anchor.filter(|a| *a != t.caret) else {
            return false;
        };
        let (s, e) = (a.min(t.caret), a.max(t.caret));
        self.splice(t.id, s, e - s, "");
        if let Some(t) = &mut self.text {
            t.caret = s;
            t.anchor = None;
        }
        true
    }

    /// A typed character (`WM_CHAR`) into the edited label; control characters are ignored.
    /// `false` when no label is edited.
    pub fn type_char(&mut self, c: char) -> bool {
        if self.text.is_none() {
            return false;
        }
        if c.is_control() {
            return true;
        }
        self.insert(&c.to_string());
        true
    }

    fn insert(&mut self, s: &str) {
        self.delete_range();
        let Some(t) = self.text.clone() else {
            return;
        };
        let at = t.comp.map_or(t.caret, |(start, _)| start);
        let len = t.comp.map_or(0, |(_, l)| l);
        self.splice(t.id, at, len, s);
        if let Some(t) = &mut self.text {
            t.caret = at + s.chars().count();
            t.comp = None;
            t.anchor = None;
        }
    }

    /// An editing key in the label; `false` when no label is edited.
    pub fn text_key(&mut self, k: TextKey) -> bool {
        let Some(t) = self.text.clone() else {
            return false;
        };
        if t.comp.is_some() {
            // The input method owns the keys during a composition.
            return true;
        }
        let n = self.text_of(t.id).map_or(0, |x| x.text.chars().count());
        let line_start = |caret: usize| {
            self.text_of(t.id).map_or(0, |x| {
                x.text
                    .chars()
                    .take(caret)
                    .collect::<Vec<_>>()
                    .iter()
                    .rposition(|c| *c == '\n')
                    .map_or(0, |i| i + 1)
            })
        };
        let line_end = |caret: usize| {
            self.text_of(t.id).map_or(n, |x| {
                x.text
                    .chars()
                    .skip(caret)
                    .position(|c| c == '\n')
                    .map_or(n, |i| caret + i)
            })
        };
        match k {
            TextKey::Newline => self.insert("\n"),
            TextKey::Backspace => {
                if !self.delete_range() && t.caret > 0 {
                    self.splice(t.id, t.caret - 1, 1, "");
                    if let Some(e) = &mut self.text {
                        e.caret -= 1;
                    }
                }
            }
            TextKey::Delete => {
                if !self.delete_range() && t.caret < n {
                    self.splice(t.id, t.caret, 1, "");
                }
            }
            TextKey::Left | TextKey::Right | TextKey::Home | TextKey::End => {
                let caret = match k {
                    TextKey::Left => t.caret.saturating_sub(1),
                    TextKey::Right => (t.caret + 1).min(n),
                    TextKey::Home => line_start(t.caret),
                    _ => line_end(t.caret),
                };
                if let Some(e) = &mut self.text {
                    e.caret = caret;
                    e.anchor = None;
                }
                self.mark_object(t.id);
            }
        }
        true
    }

    /// An input-method event for the edited label (Korean first, NOEUL-NFR-014): the
    /// composition is shown at the caret and becomes text only when committed. `false` when no
    /// label is edited.
    pub fn ime(&mut self, e: Ime) -> bool {
        if self.text.is_none() {
            return false;
        }
        match e {
            Ime::Start => {
                self.delete_range();
            }
            Ime::Compose(s) => {
                self.delete_range();
                let Some(t) = self.text.clone() else {
                    return true;
                };
                let (start, len) = t.comp.unwrap_or((t.caret, 0));
                self.splice(t.id, start, len, &s);
                let n = s.chars().count();
                if let Some(e) = &mut self.text {
                    e.comp = (n > 0).then_some((start, n));
                    e.caret = start + n;
                }
            }
            Ime::Commit(s) => self.insert(&s),
            Ime::End => {
                if let Some(t) = self.text.clone()
                    && let Some((start, len)) = t.comp
                {
                    self.splice(t.id, start, len, "");
                    if let Some(e) = &mut self.text {
                        e.comp = None;
                        e.caret = start;
                    }
                }
            }
        }
        true
    }

    /// Selects the word around `at` in the edited label (a double-click, FLOW-14).
    pub fn select_word_at(&mut self, at: (i32, i32)) -> bool {
        if self.text.is_none() {
            return false;
        }
        self.caret_at(at);
        let Some(t) = self.text.clone() else {
            return false;
        };
        let chars: Vec<char> = self
            .text_of(t.id)
            .map(|x| x.text.chars().collect())
            .unwrap_or_default();
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let mut s = t.caret.min(chars.len());
        let mut e = s;
        while s > 0 && word(chars[s - 1]) {
            s -= 1;
        }
        while e < chars.len() && word(chars[e]) {
            e += 1;
        }
        if let Some(x) = &mut self.text {
            x.anchor = Some(s);
            x.caret = e;
        }
        self.mark_object(t.id);
        true
    }

    /// Puts the caret of the edited label at the character boundary nearest to `at`.
    fn caret_at(&mut self, at: (i32, i32)) {
        let Some(t) = self.text.clone() else {
            return;
        };
        let Some(x) = self.text_of(t.id) else {
            return;
        };
        let Some(g) = x.glyphs.clone() else {
            return;
        };
        let lh = g.line_height.max(1) as i32;
        let line = ((at.1 - x.at.1) / lh).clamp(0, g.carets.len() as i32 - 1) as usize;
        let carets = &g.carets[line];
        let dx = (at.0 - x.at.0).max(0) as u32;
        let col = carets
            .iter()
            .enumerate()
            .min_by_key(|(_, c)| c.abs_diff(dx))
            .map_or(0, |(i, _)| i);
        let before: usize = g.carets[..line].iter().map(|c| c.len()).sum();
        if let Some(e) = &mut self.text {
            e.caret = before + col;
            e.anchor = None;
        }
        self.mark_object(t.id);
    }

    /// Where the caret of the edited label is drawn (device pixels), and the selected range
    /// and the composition as rectangles, for the overlay to show.
    pub fn text_marks(&self) -> Option<TextMarks> {
        let t = self.text.as_ref()?;
        let x = self.text_of(t.id)?;
        let size = self.doc.get(t.id)?.text_size();
        let g = x.glyphs.as_ref()?;
        let pos = |i: usize| caret_pos(g, i);
        let lh = g.line_height;
        let (cx, cy) = pos(t.caret);
        let caret = Rect::new(
            x.at.0 + cx as i32,
            x.at.1 + cy as i32,
            (size / 14).max(1),
            lh.max(1),
        );
        let range = |s: usize, e: usize| -> Vec<Rect> {
            let mut out = Vec::new();
            let mut i = s;
            while i < e {
                let (x0, y0) = pos(i);
                // to the end of this line or the range
                let mut j = i;
                while j < e && pos(j + 1).1 == y0 {
                    j += 1;
                }
                let (x1, _) = pos(j);
                if x1 > x0 {
                    out.push(Rect::new(
                        x.at.0 + x0 as i32,
                        x.at.1 + y0 as i32,
                        x1 - x0,
                        lh,
                    ));
                }
                i = j + 1;
            }
            out
        };
        let selection = t
            .anchor
            .filter(|a| *a != t.caret)
            .map(|a| range(a.min(t.caret), a.max(t.caret)))
            .unwrap_or_default();
        let composition = t
            .comp
            .map(|(s, l)| {
                range(s, s + l)
                    .into_iter()
                    .map(|r| {
                        let th = (size / 16).max(1);
                        Rect::new(r.x, r.y + r.height as i32 - th as i32, r.width, th)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(TextMarks {
            caret,
            selection,
            composition,
        })
    }

    /// Typesets every label whose text or size changed (the overlay calls it after an input
    /// with the platform's typesetter).
    pub fn typeset(&mut self, ts: &dyn Typesetter) {
        let needs = |o: &Object| {
            matches!(&o.shape, Shape::Text(t)
                if t.glyphs.as_ref().is_none_or(|g| g.text != t.text || g.size != o.text_size()))
        };
        if !self.retype && !self.doc.objects.iter().any(needs) {
            return;
        }
        self.retype = false;
        let ids: Vec<u32> = self
            .doc
            .objects
            .iter()
            .filter(|o| needs(o))
            .map(|o| o.id)
            .collect();
        for id in ids {
            self.mark_object(id);
            if let Some(i) = self.doc.index(id) {
                let size = self.doc.objects[i].text_size();
                if let Shape::Text(t) = &mut self.doc.objects[i].shape {
                    t.glyphs = Some(Arc::new(ts.typeset(&t.text, size)));
                }
            }
            self.mark_object(id);
        }
        // The undo history keeps glyphs too, so undoing never needs the typesetter.
        for state in self.doc.undo.iter_mut().chain(self.doc.redo.iter_mut()) {
            for o in state.iter_mut() {
                let size = o.text_size();
                if let Shape::Text(t) = &mut o.shape
                    && t.glyphs
                        .as_ref()
                        .is_none_or(|g| g.text != t.text || g.size != size)
                {
                    t.glyphs = Some(Arc::new(ts.typeset(&t.text, size)));
                }
            }
        }
    }
}

/// Where the caret before character `i` (counted over all lines) sits in a label's glyphs.
fn caret_pos(g: &Glyphs, i: usize) -> (u32, u32) {
    let mut rest = i;
    for (line, carets) in g.carets.iter().enumerate() {
        if rest < carets.len() {
            return (carets[rest], line as u32 * g.line_height);
        }
        // `carets.len()` positions on this line; the newline itself is one character.
        rest -= carets.len();
    }
    let last = g.carets.len().saturating_sub(1);
    (
        g.carets.last().and_then(|c| c.last()).copied().unwrap_or(0),
        last as u32 * g.line_height,
    )
}

/// The caret, selection and composition of the label being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMarks {
    pub caret: Rect,
    pub selection: Vec<Rect>,
    /// The composition's underline.
    pub composition: Vec<Rect>,
}

impl Object {
    /// The box of a box-like shape (the outline's outer edge; the points' box for strokes).
    fn bounds_of_shape(&self) -> Bounds {
        match &self.shape {
            Shape::Rectangle(b) | Shape::Ellipse(b) => *b,
            Shape::Pen(p) | Shape::Marker(p) => {
                Bounds::around(p).unwrap_or(Bounds::new(0.0, 0.0, 0.0, 0.0))
            }
            Shape::Line(a, b) | Shape::Arrow(a, b) => Bounds::new(a.x, a.y, b.x, b.y),
            Shape::Text(_) => self.bounds(),
        }
    }
}

/// The handles of an object: the ends of a line or arrow, eight on the box otherwise (a label
/// has none: its size follows its level).
pub fn handles(o: &Object) -> Vec<(Handle, P)> {
    let bx = |l, r, t, b| Handle::Box {
        left: l,
        right: r,
        top: t,
        bottom: b,
    };
    match &o.shape {
        Shape::Line(a, b) | Shape::Arrow(a, b) => vec![(Handle::Start, *a), (Handle::End, *b)],
        Shape::Text(_) => Vec::new(),
        _ => {
            let b = o.bounds_of_shape();
            let (xm, ym) = ((b.l + b.r) / 2.0, (b.t + b.b) / 2.0);
            vec![
                (bx(true, false, true, false), P::new(b.l, b.t)),
                (bx(false, true, true, false), P::new(b.r, b.t)),
                (bx(true, false, false, true), P::new(b.l, b.b)),
                (bx(false, true, false, true), P::new(b.r, b.b)),
                (bx(false, false, true, false), P::new(xm, b.t)),
                (bx(false, false, false, true), P::new(xm, b.b)),
                (bx(true, false, false, false), P::new(b.l, ym)),
                (bx(false, true, false, false), P::new(b.r, ym)),
            ]
        }
    }
}

/// `o` with its box `from` stretched to `to`: a box shape takes it, a stroke's points are
/// scaled into it.
fn reshaped(o: &Object, from: Bounds, to: Bounds) -> Object {
    let to = Bounds::new(to.l, to.t, to.r, to.b);
    let map = |p: &P| {
        let fx = if from.width() > 0.0 {
            (p.x - from.l) / from.width()
        } else {
            0.0
        };
        let fy = if from.height() > 0.0 {
            (p.y - from.t) / from.height()
        } else {
            0.0
        };
        P::new(to.l + fx * to.width(), to.t + fy * to.height())
    };
    let round = |b: Bounds| Bounds::new(b.l.round(), b.t.round(), b.r.round(), b.b.round());
    let shape = match &o.shape {
        Shape::Rectangle(_) => Shape::Rectangle(round(to)),
        Shape::Ellipse(_) => Shape::Ellipse(round(to)),
        Shape::Pen(p) => Shape::Pen(p.iter().map(map).collect()),
        Shape::Marker(p) => Shape::Marker(p.iter().map(map).collect()),
        other => other.clone(),
    };
    Object { shape, ..o.clone() }
}

/// `start` with `handle` dragged to pixel `at`.
fn resized(start: &Object, handle: Handle, at: (i32, i32)) -> Object {
    match (handle, &start.shape) {
        (Handle::Start | Handle::End, Shape::Line(a, b) | Shape::Arrow(a, b)) => {
            let p = P::of_pixel(at);
            let (a, b) = if handle == Handle::Start {
                (p, *b)
            } else {
                (*a, p)
            };
            Object {
                shape: if matches!(start.shape, Shape::Line(..)) {
                    Shape::Line(a, b)
                } else {
                    Shape::Arrow(a, b)
                },
                ..start.clone()
            }
        }
        (
            Handle::Box {
                left,
                right,
                top,
                bottom,
            },
            _,
        ) => {
            let b = start.bounds_of_shape();
            let mut nb = b;
            let (x, y) = (at.0 as f32, at.1 as f32);
            if left {
                nb.l = x;
            }
            if right {
                nb.r = x + 1.0;
            }
            if top {
                nb.t = y;
            }
            if bottom {
                nb.b = y + 1.0;
            }
            reshaped(start, b, Bounds::new(nb.l, nb.t, nb.r, nb.b))
        }
        _ => start.clone(),
    }
}

// ----- geometry -----

fn segment_distance(p: P, a: P, b: P) -> f32 {
    let ab = b.sub(a);
    let l2 = ab.dot(ab);
    if l2 == 0.0 {
        return p.sub(a).len();
    }
    let t = (p.sub(a).dot(ab) / l2).clamp(0.0, 1.0);
    p.sub(a.add(ab.scale(t))).len()
}

fn polyline_distance(p: P, pts: &[P]) -> f32 {
    match pts {
        [] => f32::INFINITY,
        [one] => p.sub(*one).len(),
        _ => pts
            .windows(2)
            .map(|w| segment_distance(p, w[0], w[1]))
            .fold(f32::INFINITY, f32::min),
    }
}

/// The centre and the centre-line radii of an ellipse outline `w` wide inside `b`.
fn ellipse_axes(b: Bounds, w: f32) -> (P, f32, f32) {
    let c = P::new((b.l + b.r) / 2.0, (b.t + b.b) / 2.0);
    (c, b.width() / 2.0 - w / 2.0, b.height() / 2.0 - w / 2.0)
}

/// The approximate signed distance from `d` (relative to the centre) to the ellipse with radii
/// `a`, `b` (negative inside).
fn ellipse_distance(d: P, a: f32, b: f32) -> f32 {
    let g = d.x * d.x / (a * a) + d.y * d.y / (b * b) - 1.0;
    let gx = 2.0 * d.x / (a * a);
    let gy = 2.0 * d.y / (b * b);
    let n = gx.hypot(gy);
    if n < 1e-6 {
        return -a.min(b);
    }
    g / n
}

// ----- pixels -----

/// The coverage of one object over a box of pixels.
struct Mask {
    rect: Rect,
    cov: Vec<u8>,
}

impl Mask {
    fn new(rect: Rect) -> Mask {
        Mask {
            rect,
            cov: vec![0; rect.width as usize * rect.height as usize],
        }
    }

    fn put(&mut self, x: i64, y: i64, c: f32) {
        let (rx, ry) = (i64::from(self.rect.x), i64::from(self.rect.y));
        if x < rx || y < ry || x >= self.rect.right() || y >= self.rect.bottom() {
            return;
        }
        let i = (y - ry) as usize * self.rect.width as usize + (x - rx) as usize;
        let v = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        if v > self.cov[i] {
            self.cov[i] = v;
        }
    }

    /// Pixels of `b` (edges) inside the mask, as whole-pixel ranges.
    fn span(&self, b: Bounds) -> Option<(i64, i64, i64, i64)> {
        let r = b.to_rect()?;
        let i = crate::overlay::intersection(r, self.rect)?;
        Some((i64::from(i.x), i64::from(i.y), i.right(), i.bottom()))
    }

    /// A round-capped segment `w` wide.
    fn capsule(&mut self, a: P, b: P, half: f32) {
        let bb = Bounds::new(a.x, a.y, b.x, b.y).grow(half + 1.0);
        let Some((x0, y0, x1, y1)) = self.span(bb) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let c = P::new(x as f32 + 0.5, y as f32 + 0.5);
                let d = segment_distance(c, a, b);
                if d < half + 0.5 {
                    self.put(x, y, half + 0.5 - d);
                }
            }
        }
    }

    fn polyline(&mut self, pts: &[P], half: f32) {
        match pts {
            [] => {}
            [one] => self.capsule(*one, *one, half),
            _ => {
                for w in pts.windows(2) {
                    self.capsule(w[0], w[1], half);
                }
            }
        }
    }

    /// An axis-aligned outline `w` whole pixels thick inside `b` (crisp).
    fn rect_outline(&mut self, b: Bounds, w: f32) {
        let r = |v: f32| v.round() as i64;
        let (l, t, rr, bb) = (r(b.l), r(b.t), r(b.r), r(b.b));
        let w = (w.round() as i64).max(1);
        for y in t..bb {
            for x in l..rr {
                if x < l + w || x >= rr - w || y < t + w || y >= bb - w {
                    self.put(x, y, 1.0);
                }
            }
        }
    }

    fn ellipse(&mut self, b: Bounds, w: f32) {
        let (c, a, bb) = ellipse_axes(b, w);
        let half = w / 2.0;
        if a <= 0.5 || bb <= 0.5 {
            // Too small for a ring: a filled ellipse of the box.
            let (ra, rb) = (b.width() / 2.0, b.height() / 2.0);
            let Some((x0, y0, x1, y1)) = self.span(b.grow(1.0)) else {
                return;
            };
            for y in y0..y1 {
                for x in x0..x1 {
                    let d = P::new(x as f32 + 0.5, y as f32 + 0.5).sub(c);
                    let dd = if ra > 0.0 && rb > 0.0 {
                        ellipse_distance(d, ra, rb)
                    } else {
                        1.0
                    };
                    self.put(x, y, 0.5 - dd);
                }
            }
            return;
        }
        let Some((x0, y0, x1, y1)) = self.span(b.grow(1.0)) else {
            return;
        };
        let (oa, ob) = (a + half + 1.0, bb + half + 1.0);
        let (ia, ib) = (a - half - 1.0, bb - half - 1.0);
        for y in y0..y1 {
            let dy = y as f32 + 0.5 - c.y;
            if dy.abs() > ob {
                continue;
            }
            let xo = oa * (1.0 - (dy / ob).powi(2)).max(0.0).sqrt();
            let xi = if ia > 0.0 && ib > 0.0 && dy.abs() < ib {
                ia * (1.0 - (dy / ib).powi(2)).max(0.0).sqrt()
            } else {
                -1.0
            };
            let mut visit = |from: f32, to: f32| {
                let fx = ((c.x + from).floor() as i64).max(x0);
                let tx = ((c.x + to).ceil() as i64).min(x1);
                for x in fx..tx {
                    let d = P::new(x as f32 + 0.5, y as f32 + 0.5).sub(c);
                    let dist = ellipse_distance(d, a, bb).abs();
                    if dist < half + 0.5 {
                        self.put(x, y, half + 0.5 - dist);
                    }
                }
            };
            if xi > 0.0 {
                visit(-xo, -xi);
                visit(xi, xo);
            } else {
                visit(-xo, xo);
            }
        }
    }

    /// A filled triangle (anti-aliased edges).
    fn triangle(&mut self, p: [P; 3]) {
        let Some(bb) = Bounds::around(&p) else {
            return;
        };
        let Some((x0, y0, x1, y1)) = self.span(bb.grow(1.0)) else {
            return;
        };
        // Edge normals pointing inside.
        let area = (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[1].y - p[0].y) * (p[2].x - p[0].x);
        if area.abs() < 1e-3 {
            return;
        }
        let sign = area.signum();
        let edges: Vec<(P, P, f32)> = (0..3)
            .map(|i| {
                let a = p[i];
                let b = p[(i + 1) % 3];
                let e = b.sub(a);
                let n = P::new(-e.y * sign, e.x * sign);
                let l = n.len();
                (a, n.scale(1.0 / l), l)
            })
            .collect();
        for y in y0..y1 {
            for x in x0..x1 {
                let c = P::new(x as f32 + 0.5, y as f32 + 0.5);
                let d = edges
                    .iter()
                    .map(|(a, n, _)| c.sub(*a).dot(*n))
                    .fold(f32::INFINITY, f32::min);
                if d > -0.5 {
                    self.put(x, y, d + 0.5);
                }
            }
        }
    }

    fn glyphs(&mut self, at: (i32, i32), g: &Glyphs) {
        let Some(r) =
            crate::overlay::intersection(Rect::new(at.0, at.1, g.width, g.height), self.rect)
        else {
            return;
        };
        for y in r.y..r.y + r.height as i32 {
            for x in r.x..r.x + r.width as i32 {
                let gi = (y - at.1) as usize * g.width as usize + (x - at.0) as usize;
                let v = g.mask[gi];
                if v > 0 {
                    self.put(i64::from(x), i64::from(y), f32::from(v) / 255.0);
                }
            }
        }
    }
}

/// The coverage of `o` inside `clip`, if it touches it.
fn mask_of(o: &Object, clip: Rect) -> Option<Mask> {
    let area = crate::overlay::intersection(o.bounds().to_rect()?, clip)?;
    let mut m = Mask::new(area);
    let w = o.width();
    match &o.shape {
        Shape::Rectangle(b) => m.rect_outline(*b, w),
        Shape::Ellipse(b) => m.ellipse(*b, w),
        Shape::Line(a, b) => m.capsule(*a, *b, w / 2.0),
        Shape::Arrow(a, b) => {
            let d = b.sub(*a);
            let len = d.len();
            let (mut hl, mut hw) = arrow_head(w);
            if len > 0.0 {
                if hl > len {
                    hw *= len / hl;
                    hl = len;
                }
                let u = d.scale(1.0 / len);
                let n = P::new(-u.y, u.x);
                let base = b.sub(u.scale(hl));
                // The shaft ends inside the head so they join without a gap.
                m.capsule(*a, b.sub(u.scale(hl * 0.6)), w / 2.0);
                m.triangle([*b, base.add(n.scale(hw)), base.sub(n.scale(hw))]);
            } else {
                m.capsule(*a, *b, w / 2.0);
            }
        }
        Shape::Pen(pts) | Shape::Marker(pts) => m.polyline(pts, w / 2.0),
        Shape::Text(t) => {
            if let Some(g) = &t.glyphs {
                m.glyphs(t.at, g);
            }
        }
    }
    Some(m)
}

/// Source-over of `c` at coverage `cov` onto one straight-alpha BGRA pixel.
#[inline]
fn over(dst: &mut [u8; 4], c: Rgba, cov: u8) {
    let sa = (u32::from(c.a) * u32::from(cov) + 127) / 255;
    if sa == 0 {
        return;
    }
    let src = [u32::from(c.b), u32::from(c.g), u32::from(c.r)];
    let da = u32::from(dst[3]);
    if da == 255 {
        for k in 0..3 {
            dst[k] = ((src[k] * sa + u32::from(dst[k]) * (255 - sa) + 127) / 255) as u8;
        }
        return;
    }
    if sa == 255 {
        dst[0] = c.b;
        dst[1] = c.g;
        dst[2] = c.r;
        dst[3] = 255;
        return;
    }
    // A transparent picture: the colour over what shows of it.
    let da_eff = da * (255 - sa); // × 255
    let out_a = sa * 255 + da_eff; // × 255
    if out_a == 0 {
        return;
    }
    for k in 0..3 {
        let v = (src[k] * sa * 255 + u32::from(dst[k]) * da_eff + out_a / 2) / out_a;
        dst[k] = v.min(255) as u8;
    }
    dst[3] = ((out_a + 127) / 255).min(255) as u8;
}

/// Draws `objects` (in order) into `buf`, a straight-alpha BGRA buffer of the picture part
/// `rect`, only inside `clip`.
pub fn draw(objects: &[Object], buf: &mut [u8], rect: Rect, clip: Rect) {
    let Some(clip) = crate::overlay::intersection(clip, rect) else {
        return;
    };
    if buf.len() < rect.bgra_bytes() as usize {
        return;
    }
    let stride = rect.width as usize * 4;
    for o in objects {
        let Some(m) = mask_of(o, clip) else {
            continue;
        };
        let c = if matches!(o.shape, Shape::Marker(_)) {
            Rgba {
                a: ((u32::from(o.color.a) * u32::from(MARKER_ALPHA)) / 255) as u8,
                ..o.color
            }
        } else {
            o.color
        };
        let (mx, my) = (
            (i64::from(m.rect.x) - i64::from(rect.x)) as usize,
            (i64::from(m.rect.y) - i64::from(rect.y)) as usize,
        );
        for row in 0..m.rect.height as usize {
            let line = &m.cov[row * m.rect.width as usize..(row + 1) * m.rect.width as usize];
            let start = (my + row) * stride + mx * 4;
            let out = &mut buf[start..start + m.rect.width as usize * 4];
            for (px, cov) in out.as_chunks_mut::<4>().0.iter_mut().zip(line) {
                if *cov > 0 {
                    over(px, c, *cov);
                }
            }
        }
    }
}

/// The part `area` of `base` with `objects` drawn on it: what copy, save and a new pin output
/// (NOEUL-FR-081: annotations become pixels here, and only here). `None` when `area` is not
/// inside `base`.
pub fn flatten(base: &Frame, objects: &[Object], area: Rect) -> Option<Frame> {
    let mut f = base.crop(area)?;
    if !objects.is_empty() {
        let r = f.rect();
        draw(objects, f.pixels_mut(), r, r);
    }
    Some(f)
}

/// Re-renders `area` of `layer` (a part of `base` with `objects` drawn on it, as [`flatten`]
/// made it): the picture's pixels there again, then the objects over them. The overlay calls it
/// with the pixels an edit changed.
pub fn refresh(layer: &mut Frame, base: &Frame, objects: &[Object], area: Rect) {
    let lr = layer.rect();
    let Some(a) = crate::overlay::intersection(area, lr)
        .and_then(|a| crate::overlay::intersection(a, base.rect()))
    else {
        return;
    };
    let br = base.rect();
    let (ls, bs) = (lr.width as usize * 4, br.width as usize * 4);
    let row = a.width as usize * 4;
    let src = base.pixels();
    let dst = layer.pixels_mut();
    for y in a.y..a.y + a.height as i32 {
        let li = (i64::from(y) - i64::from(lr.y)) as usize * ls
            + (i64::from(a.x) - i64::from(lr.x)) as usize * 4;
        let bi = (i64::from(y) - i64::from(br.y)) as usize * bs
            + (i64::from(a.x) - i64::from(br.x)) as usize * 4;
        dst[li..li + row].copy_from_slice(&src[bi..bi + row]);
    }
    draw(objects, layer.pixels_mut(), lr, a);
}

/// `objects` moved by `(dx, dy)`: from screen pixels to a pin's image and back.
pub fn translate_all(objects: &[Object], dx: i32, dy: i32) -> Vec<Object> {
    objects.iter().map(|o| o.translated(dx, dy)).collect()
}

/// The objects that show something inside `area` (an output keeps only those).
pub fn inside(objects: &[Object], area: Rect) -> Vec<Object> {
    objects
        .iter()
        .filter(|o| {
            o.bounds()
                .to_rect()
                .is_some_and(|b| crate::overlay::intersection(b, area).is_some())
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
